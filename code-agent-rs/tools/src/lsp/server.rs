//! LSP 服务器生命周期管理
//!
//! 【领域含义】管理 LSP 服务器子进程的完整生命周期：启动、初始化握手、请求分发、关闭。
//! 每个语言对应一个独立的服务器实例，每个实例拥有自己的 stdin/stdout JSON-RPC 通道和后台读取任务。
//!
//! 【核心职责】`LspServerManager` 负责服务器的创建、状态跟踪、请求路由和资源清理。

use crate::lsp::config::SUPPORTED_LANGUAGES;
use crate::lsp::requests::{
    self, build_notification, build_request, read_message, write_message, write_json_message,
    ClientCapabilities, InitializeParams, InitializeResult, LspMessage, RequestId,
    ServerCapabilities,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// LSP 服务器管理错误
///
/// 【领域含义】LSP 服务器生命周期管理过程中可能出现的所有错误类型。
/// 【核心职责】统一封装语言不支持、启动失败、崩溃、I/O 错误、协议错误、超时等异常。
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// 不支持的语言
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),

    /// 服务器二进制文件未找到或无法执行
    #[error("failed to start LSP server for '{language}': {source}")]
    ServerStart {
        language: String,
        #[source]
        source: std::io::Error,
    },

    /// 服务器进程意外退出
    #[error("LSP server for '{language}' crashed: {reason}")]
    ServerCrashed { language: String, reason: String },

    /// 与服务器通信的 I/O 错误
    #[error("I/O error for '{language}': {source}")]
    Io {
        language: String,
        #[source]
        source: std::io::Error,
    },

    /// 服务器返回了错误响应
    #[error("LSP error for '{language}' (code {code}): {message}")]
    ProtocolError {
        language: String,
        code: i64,
        message: String,
    },

    /// 服务器未在超时时间内响应
    #[error("LSP server for '{language}' timed out")]
    Timeout { language: String },

    /// 服务器返回了无效的初始化响应
    #[error("LSP server for '{language}' returned invalid initialize response")]
    InvalidInitializeResponse { language: String },

    /// 服务器未运行
    #[error("LSP server for '{language}' is not running")]
    NotRunning { language: String },
}

// ---------------------------------------------------------------------------
// Server status
// ---------------------------------------------------------------------------

/// LSP 服务器状态
///
/// 【领域含义】表示 LSP 服务器实例的当前生命周期阶段。
/// 【核心职责】供外部查询服务器状态，决定是否可发送请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    /// 未启动
    NotStarted,
    /// 初始化中（正在执行 initialize 握手）
    Initializing,
    /// 运行中（可接收请求）
    Running,
    /// 已崩溃（进程意外退出）
    Crashed,
}

// ---------------------------------------------------------------------------
// LSP server handle (internal)
// ---------------------------------------------------------------------------

/// 标识一个等待响应的待处理请求
type PendingRequest = oneshot::Sender<Result<Value, LspError>>;

/// 运行中的 LSP 服务器句柄
///
/// 【领域含义】封装一个 LSP 服务器子进程的所有运行时状态。
/// 【核心职责】持有子进程、能力信息、状态、请求通道和后台读取任务句柄。
struct LspServerHandle {
    /// 子进程
    process: Child,
    /// 初始化时报告的服务能力
    capabilities: ServerCapabilities,
    /// 当前状态
    status: ServerStatus,
    /// 向后台 I/O 任务发送请求的通道
    request_tx: mpsc::UnboundedSender<(serde_json::Value, PendingRequest)>,
    /// 后台读取任务的 JoinHandle
    read_task: tokio::task::JoinHandle<()>,
}

// ---------------------------------------------------------------------------
// LSP Server Manager
// ---------------------------------------------------------------------------

/// LSP 服务器管理器
///
/// 【领域含义】管理多个 LSP 服务器子进程的生命周期，每种语言对应一个独立实例。
/// 【核心职责】处理服务器的启动、停止、状态监控和请求路由。
pub struct LspServerManager {
    /// 语言 → 服务器句柄的映射
    servers: HashMap<String, LspServerHandle>,
}

impl LspServerManager {
    /// 创建空的服务器管理器
    ///
    /// 【领域含义】初始化一个没有任何运行中服务器的管理器。
    /// 【核心职责】创建空的 `HashMap` 用于存储语言到服务器句柄的映射。
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// 启动指定语言的 LSP 服务器
    ///
    /// 【领域含义】为指定语言启动 LSP 服务器进程，执行初始化握手，启动后台读取任务。
    /// 【核心职责】查找语言配置 → 派生进程 → 执行 initialize 握手 → 创建请求通道 → 启动后台读取循环。
    ///
    /// # 错误
    ///
    /// 语言不支持、服务器二进制文件未找到或初始化握手失败时返回错误。
    pub async fn start_server(&mut self, language: &str, root_uri: &Path) -> Result<(), LspError> {
        // Check if already running
        if self.servers.contains_key(language) {
            let status = self.servers[language].status;
            if status == ServerStatus::Running || status == ServerStatus::Initializing {
                debug!("LSP server for '{language}' already running");
                return Ok(());
            }
        }

        // Look up config
        let config = SUPPORTED_LANGUAGES
            .iter()
            .find(|c| c.language == language)
            .ok_or_else(|| LspError::UnsupportedLanguage(language.to_string()))?;

        info!("Starting LSP server: {} (via {})", language, config.command);

        // Spawn the process
        let mut cmd = Command::new(config.command);
        cmd.args(config.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // Prevent the child process from being killed with the parent on Unix
        // (this is the default on Windows).
        cmd.kill_on_drop(true);

        // P0-3: 清除敏感环境变量，防止 LLM_API_KEY 等凭据泄露给 LSP 子进程
        crate::filter_sensitive_env(&mut cmd);

        let mut child = cmd.spawn().map_err(|source| LspError::ServerStart {
            language: language.to_string(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or_else(|| LspError::ServerStart {
            language: language.to_string(),
            source: std::io::Error::other("no stdin"),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| LspError::ServerStart {
            language: language.to_string(),
            source: std::io::Error::other("no stdout"),
        })?;

        let mut writer = BufWriter::new(stdin);
        let mut reader = BufReader::new(stdout);

        // Perform initialize handshake
        let capabilities = Self::do_initialize(
            &mut writer,
            &mut reader,
            language,
            root_uri.to_string_lossy().as_ref(),
        )
        .await?;

        info!(
            "LSP server for '{language}' initialized with capabilities: {:?}",
            capabilities
        );

        // Create channel for request dispatch
        let (request_tx, mut request_rx) =
            mpsc::unbounded_channel::<(serde_json::Value, PendingRequest)>();

        // Spawn background read task
        let lang = language.to_string();
        let read_task = tokio::spawn(async move {
            let mut next_id: i64 = 1;
            let mut pending: HashMap<RequestId, PendingRequest> = HashMap::new();

            loop {
                tokio::select! {
                    // Process incoming requests from LspClient
                    maybe_req = request_rx.recv() => {
                        match maybe_req {
                            Some((req_value, responder)) => {
                                let id = RequestId::Number(next_id);
                                next_id += 1;

                                // Inject the id into the request value
                                let mut req_with_id = req_value;
                                if let Some(obj) = req_with_id.as_object_mut() {
                                    obj.insert("id".to_string(), serde_json::to_value(&id).unwrap());
                                }

                                pending.insert(id.clone(), responder);

                                let lang = lang.clone();
                                if let Err(e) = write_json_message(&mut writer, &req_with_id).await {
                                    error!("Failed to write request to LSP server '{}': {}", lang, e);
                                    // Convert to string so we can use it in the loop
                                    let err_msg = format!("{e}");
                                    // Clean up all pending requests since the channel is broken
                                    for (_, rx) in pending.drain() {
                                        let _ = rx.send(Err(LspError::ServerCrashed {
                                            language: lang.clone(),
                                            reason: err_msg.clone(),
                                        }));
                                    }
                                    return;
                                }
                            }
                            None => {
                                // Channel closed → shut down
                                debug!("Request channel closed for LSP server '{}'", lang);
                                return;
                            }
                        }
                    }

                    // Read response from server stdout
                    result = read_message(&mut reader) => {
                        match result {
                            Ok(msg) => {
                                match msg {
                                    LspMessage::Response(resp) => {
                                        if let Some(responder) = pending.remove(&resp.id) {
                                            if let Some(err) = resp.error {
                                                let _ = responder.send(Err(LspError::ProtocolError {
                                                    language: lang.clone(),
                                                    code: err.code,
                                                    message: err.message,
                                                }));
                                            } else {
                                                let _ = responder.send(Ok(resp.result.unwrap_or(Value::Null)));
                                            }
                                        }
                                    }
                                    LspMessage::Request(req) if req.id.is_none() => {
                                        // Notification from server (e.g., publishDiagnostics)
                                        // For now we just log it — full notification
                                        // routing can be added later.
                                        debug!(
                                            "LSP notification '{}' from '{}': {:?}",
                                            req.method, lang, req.params
                                        );
                                    }
                                    LspMessage::Request(req) => {
                                        // Server-initiated request — respond with a "method not found" error
                                        if let Some(id) = req.id {
                                            let resp = requests::build_response(id, None);
                                            if let Err(e) = write_message(&mut writer, &resp).await {
                                                warn!("Failed to respond to server request: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                    debug!("LSP server '{}' stdout closed", lang);
                                } else {
                                    error!("Error reading from LSP server '{}': {}", lang, e);
                                }
                                // Drain all pending requests
                                for (_, rx) in pending.drain() {
                                    let _ = rx.send(Err(LspError::ServerCrashed {
                                        language: lang.clone(),
                                        reason: e.to_string(),
                                    }));
                                }
                                return;
                            }
                        }
                    }
                }
            }
        });

        self.servers.insert(
            language.to_string(),
            LspServerHandle {
                process: child,
                capabilities,
                status: ServerStatus::Running,
                request_tx,
                read_task,
            },
        );

        Ok(())
    }

    /// 停止 LSP 服务器并清理资源
    ///
    /// 【领域含义】优雅关闭指定语言的 LSP 服务器，发送 shutdown 通知，强制终止进程。
    /// 【核心职责】发送 shutdown → 等待 3 秒优雅退出 → 强制 kill → 中止后台读取任务。
    pub async fn stop_server(&mut self, language: &str) -> Result<(), LspError> {
        if let Some(mut handle) = self.servers.remove(language) {
            info!("Stopping LSP server for '{language}'");

            // Send shutdown notification
            let shutdown = build_notification("shutdown", None);
            let stdin = handle.process.stdin.take();
            if let Some(stdin) = stdin {
                let mut writer = BufWriter::new(stdin);
                let _ = write_message(&mut writer, &shutdown).await;
            }

            // Wait briefly for graceful shutdown then kill
            tokio::time::timeout(std::time::Duration::from_secs(3), handle.process.wait())
                .await
                .ok();

            // Force kill if still running
            if handle.process.try_wait().ok().flatten().is_none() {
                let _ = handle.process.kill().await;
            }

            // Abort the read task
            handle.read_task.abort();
        }

        Ok(())
    }

    /// 获取服务器当前状态
    ///
    /// 【领域含义】查询指定语言 LSP 服务器的当前生命周期状态。
    /// 【核心职责】从 `servers` 映射中查找并返回状态，未找到时返回 `NotStarted`。
    pub fn server_status(&self, language: &str) -> ServerStatus {
        self.servers
            .get(language)
            .map(|h| h.status)
            .unwrap_or(ServerStatus::NotStarted)
    }

    /// 初始化所有已知语言的 LSP 服务器
    ///
    /// 【领域含义】遍历所有支持的语言配置，尝试启动每个语言的 LSP 服务器。
    /// 仅启动那些二进制文件在系统 PATH 中可找到的语言。
    /// 【核心职责】遍历 `SUPPORTED_LANGUAGES` → 逐个启动 → 收集成功列表。
    pub async fn init_all(&mut self, root_uri: &Path) -> Result<Vec<String>, LspError> {
        let mut started = Vec::new();

        for config in SUPPORTED_LANGUAGES.iter() {
            match self.start_server(config.language, root_uri).await {
                Ok(()) => {
                    started.push(config.language.to_string());
                }
                Err(e) => {
                    warn!(
                        "Could not start LSP server for '{}': {}",
                        config.language, e
                    );
                }
            }
        }

        Ok(started)
    }

    /// 发送 LSP 请求并等待响应
    ///
    /// 【领域含义】向运行中的 LSP 服务器发送 JSON-RPC 请求，等待响应或超时。
    /// 【核心职责】构建请求 JSON → 通过通道发送给后台任务 → 等待响应（30 秒超时）。
    pub async fn send_request(
        &self,
        language: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, LspError> {
        let handle = self
            .servers
            .get(language)
            .ok_or_else(|| LspError::NotRunning {
                language: language.to_string(),
            })?;

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        handle
            .request_tx
            .send((req, tx))
            .map_err(|_| LspError::ServerCrashed {
                language: language.to_string(),
                reason: "request channel closed".to_string(),
            })?;

        // Wait for response with a generous timeout
        tokio::time::timeout(std::time::Duration::from_secs(30), rx)
            .await
            .map_err(|_| LspError::Timeout {
                language: language.to_string(),
            })?
            .map_err(|_| LspError::ServerCrashed {
                language: language.to_string(),
                reason: "responder dropped".to_string(),
            })?
    }

    /// 获取指定语言的服务能力
    ///
    /// 【领域含义】返回 LSP 服务器在初始化时声明的能力信息。
    /// 【核心职责】从服务器句柄中提取 `ServerCapabilities` 引用。
    pub fn capabilities(&self, language: &str) -> Option<&ServerCapabilities> {
        self.servers.get(language).map(|h| &h.capabilities)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// 执行 LSP `initialize` 握手
    ///
    /// 【领域含义】与 LSP 服务器完成初始化握手：发送 initialize 请求 → 接收响应 → 发送 initialized 通知。
    /// 【核心职责】构建 InitializeParams → 发送请求 → 解析响应 → 发送 initialized 通知 → 返回能力。
    async fn do_initialize(
        writer: &mut BufWriter<tokio::process::ChildStdin>,
        reader: &mut BufReader<tokio::process::ChildStdout>,
        language: &str,
        root_uri: &str,
    ) -> Result<ServerCapabilities, LspError> {
        let root_uri = if root_uri.is_empty() {
            None
        } else {
            let uri = if root_uri.starts_with("file://") {
                root_uri.to_string()
            } else {
                format!("file:///{}", root_uri.trim_start_matches('/'))
            };
            Some(uri)
        };

        // 1. Send initialize request
        let params = InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_uri: root_uri.clone(),
            root_path: root_uri.clone(),
            capabilities: ClientCapabilities::default(),
            workspace_folders: root_uri.map(|uri| {
                vec![requests::WorkspaceFolder {
                    uri,
                    name: "workspace".to_string(),
                }]
            }),
        };

        let params_value =
            serde_json::to_value(&params).map_err(|_| LspError::InvalidInitializeResponse {
                language: language.to_string(),
            })?;

        let init_req = build_request(RequestId::Number(1), "initialize", Some(params_value));
        write_message(writer, &init_req)
            .await
            .map_err(|source| LspError::Io {
                language: language.to_string(),
                source,
            })?;

        // 2. Read initialize response
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            read_message(reader),
        )
        .await
        .map_err(|_| LspError::Timeout {
            language: language.to_string(),
        })?
        .map_err(|source| LspError::Io {
            language: language.to_string(),
            source,
        })?;

        let init_result: InitializeResult = match response {
            LspMessage::Response(ref resp) if resp.result.is_some() => {
                serde_json::from_value(resp.result.clone().unwrap()).map_err(|_| {
                    LspError::InvalidInitializeResponse {
                        language: language.to_string(),
                    }
                })?
            }
            LspMessage::Response(ref resp) if resp.error.is_some() => {
                let err = resp.error.as_ref().unwrap();
                return Err(LspError::ProtocolError {
                    language: language.to_string(),
                    code: err.code,
                    message: err.message.clone(),
                });
            }
            _ => {
                return Err(LspError::InvalidInitializeResponse {
                    language: language.to_string(),
                });
            }
        };

        // 3. Send "initialized" notification
        let initialized = build_notification("initialized", Some(serde_json::json!({})));
        write_message(writer, &initialized)
            .await
            .map_err(|source| LspError::Io {
                language: language.to_string(),
                source,
            })?;

        Ok(init_result.capabilities)
    }
}

impl Default for LspServerManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that a new manager is empty.
    #[test]
    fn new_manager_has_no_servers() {
        let mgr = LspServerManager::new();
        assert_eq!(mgr.server_status("rust"), ServerStatus::NotStarted);
        assert_eq!(mgr.server_status("python"), ServerStatus::NotStarted);
    }

    /// Test that an unknown language returns NotStarted status.
    #[test]
    fn unknown_language_not_started() {
        let mgr = LspServerManager::new();
        assert_eq!(
            mgr.server_status("nonexistent"),
            ServerStatus::NotStarted
        );
    }

    /// Test starting a server for an unsupported language returns an error.
    #[tokio::test]
    async fn unsupported_language_returns_error() {
        let mut mgr = LspServerManager::new();
        let result = mgr
            .start_server("cobol", Path::new("."))
            .await;
        assert!(result.is_err());
        match result {
            Err(LspError::UnsupportedLanguage(_)) => {}
            _ => panic!("expected UnsupportedLanguage error"),
        }
    }
}
