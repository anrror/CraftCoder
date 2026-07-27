//! 线程管理器 —— 管理多个并发 Agent 会话
//!
//! 【领域含义】`ThreadManager` 是会话线程的注册中心和生命周期管理器。
//! 一个 ThreadManager 持有多个 `Session` 实例，按 `ThreadId` 索引。
//! 提供创建、获取、移除和列出线程等生命周期操作。
//!
//! 【并发模型】Session 由 Manager 独占持有。如需跨线程并发执行轮次，
//! 可派生独立的 tokio 任务，每个任务持有各自的 Session，
//! 或使用内部可变性（例如 `Arc<Mutex<Session>>`）。
//!
//! A [`ThreadManager`] owns a set of `Session`s keyed by [`ThreadId`].
//! It provides lifecycle operations: create, get, remove, and list threads.

use std::collections::HashMap;

use code_agent_protocol::ThreadId;

use crate::agent::session::Session;

/// 线程管理器 —— Agent 会话的注册中心
///
/// 【领域含义】按 ThreadId 管理多个并发 Agent 会话。
/// 每个线程 ID 对应一个独立的 Session 实例。
/// 强制执行最大并发线程数限制。
///
/// # Example
///
/// ```rust,no_run
/// use code_agent_core::agent::{ThreadManager, Session, SessionConfig};
/// use code_agent_protocol::ThreadId;
///
/// let mut tm = ThreadManager::new(10);
/// let thread_id = ThreadId::from("thread-1");
/// // tm.create_thread(thread_id.clone(), session);
/// assert!(tm.list_threads().is_empty());
/// ```
pub struct ThreadManager {
    /// 按 ThreadId 索引的会话映射表
    threads: HashMap<ThreadId, Session>,
    /// 按 ThreadId 索引的用户 ID 映射表
    user_map: HashMap<ThreadId, String>,
    /// 最大并发线程数限制
    max_concurrent: usize,
}

impl ThreadManager {
    /// 创建新的线程管理器，指定最大并发线程数
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            threads: HashMap::new(),
            user_map: HashMap::new(),
            max_concurrent,
        }
    }

    /// 注册一个新的会话到给定线程 ID 下
    ///
    /// 【领域行为】将 Session 注册到 ThreadManager 中。
    /// 如果线程 ID 已存在或已达到最大并发限制，则返回错误。
    ///
    /// # Errors
    ///
    /// 如果线程 ID 已存在或达到最大并发线程数限制，则返回错误字符串。
    pub fn create_thread(
        &mut self,
        id: ThreadId,
        session: Session,
        user_id: String,
    ) -> Result<(), String> {
        if self.threads.len() >= self.max_concurrent {
            return Err(format!(
                "max concurrent threads ({}) reached",
                self.max_concurrent
            ));
        }
        if self.threads.contains_key(&id) {
            return Err(format!(
                "thread '{}' already exists",
                id
            ));
        }
        self.threads.insert(id.clone(), session);
        self.user_map.insert(id, user_id);
        Ok(())
    }

    /// 获取指定用户的所有线程 ID
    pub fn list_user_threads(&self, user_id: &str) -> Vec<ThreadId> {
        self.user_map
            .iter()
            .filter(|(_, uid)| uid.as_str() == user_id)
            .map(|(tid, _)| tid.clone())
            .collect()
    }

    /// 检查指定线程是否存在
    pub fn thread_exists(&self, thread_id: &ThreadId) -> bool {
        self.threads.contains_key(thread_id)
    }

    /// 检查指定用户是否拥有该线程
    ///
    /// 注意：如果线程不存在，返回 `false`。调用方应先调用 `thread_exists()`
    /// 区分「线程不存在」和「非所有者访问」两种场景。
    pub fn check_ownership(&self, thread_id: &ThreadId, user_id: &str) -> bool {
        self.user_map
            .get(thread_id)
            .map(|uid| uid.as_str() == user_id)
            .unwrap_or(false)
    }

    /// 获取给定线程 ID 对应的会话的可变引用
    pub fn get_thread(&mut self, id: &ThreadId) -> Option<&mut Session> {
        self.threads.get_mut(id)
    }

    /// 移除并返回给定线程 ID 对应的会话
    pub fn remove_thread(&mut self, id: &ThreadId) -> Option<Session> {
        self.user_map.remove(id);
        self.threads.remove(id)
    }

    /// 列出所有活跃线程 ID
    pub fn list_threads(&self) -> Vec<ThreadId> {
        self.threads.keys().cloned().collect()
    }

    /// 返回活跃线程数量
    pub fn thread_count(&self) -> usize {
        self.threads.len()
    }

    /// 如果没有注册任何线程，返回 `true`
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// 返回最大并发线程数
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionConfig;
    use crate::tools::registry::DefaultToolRegistry;
    use code_agent_protocol::{PermissionMode, SessionId, ThreadId};
    use std::sync::Arc;

    /// Create a minimal SessionConfig for testing.
    fn make_config(id: &str) -> SessionConfig {
        SessionConfig {
            id: SessionId::from(id),
            system_instructions: String::new(),
            max_iterations: 10,
            permission_mode: PermissionMode::Auto,
            model_client: Arc::new(crate::agent::session::tests::MockModelClient::new(
                vec![],
            )),
            tool_registry: Arc::new(DefaultToolRegistry::new()),
            external_cancel: None,
            max_context_tokens: None,
            temperature: None,
        }
    }

    #[tokio::test]
    async fn create_and_list_threads() {
        let mut tm = ThreadManager::new(10);

        let session1 = Session::new(make_config("sess-1")).await;
        let session2 = Session::new(make_config("sess-2")).await;

        tm.create_thread(ThreadId::from("t1"), session1, "alice".into()).unwrap();
        tm.create_thread(ThreadId::from("t2"), session2, "bob".into()).unwrap();

        let threads = tm.list_threads();
        assert_eq!(threads.len(), 2);
        assert!(threads.contains(&ThreadId::from("t1")));
        assert!(threads.contains(&ThreadId::from("t2")));

        // list_user_threads filters by user
        let alice_threads = tm.list_user_threads("alice");
        assert_eq!(alice_threads.len(), 1);
        assert!(alice_threads.contains(&ThreadId::from("t1")));

        // check_ownership
        assert!(tm.check_ownership(&ThreadId::from("t1"), "alice"));
        assert!(!tm.check_ownership(&ThreadId::from("t1"), "bob"));
    }

    #[tokio::test]
    async fn get_thread_returns_session() {
        let mut tm = ThreadManager::new(10);
        let session = Session::new(make_config("sess-1")).await;
        tm.create_thread(ThreadId::from("t1"), session, "alice".into()).unwrap();

        let s = tm.get_thread(&ThreadId::from("t1"));
        assert!(s.is_some());
        assert_eq!(s.unwrap().status().id.0, "sess-1");
    }

    #[tokio::test]
    async fn get_nonexistent_thread_returns_none() {
        let mut tm = ThreadManager::new(10);
        assert!(tm.get_thread(&ThreadId::from("no-such-thread")).is_none());
    }

    #[tokio::test]
    async fn remove_thread_returns_session() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(ThreadId::from("t1"), Session::new(make_config("sess-1")).await, "alice".into())
            .unwrap();

        let removed = tm.remove_thread(&ThreadId::from("t1"));
        assert!(removed.is_some());
        assert!(tm.get_thread(&ThreadId::from("t1")).is_none());
        assert!(tm.list_threads().is_empty());
    }

    #[tokio::test]
    async fn duplicate_thread_id_rejected() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        let result = tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-2")).await,
            "bob".into(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[tokio::test]
    async fn max_concurrent_enforced() {
        let mut tm = ThreadManager::new(2);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();
        tm.create_thread(
            ThreadId::from("t2"),
            Session::new(make_config("sess-2")).await,
            "alice".into(),
        )
        .unwrap();

        let result = tm.create_thread(
            ThreadId::from("t3"),
            Session::new(make_config("sess-3")).await,
            "bob".into(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max concurrent"));
    }

    #[tokio::test]
    async fn thread_count_and_empty() {
        let mut tm = ThreadManager::new(10);
        assert!(tm.is_empty());
        assert_eq!(tm.thread_count(), 0);

        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();
        assert!(!tm.is_empty());
        assert_eq!(tm.thread_count(), 1);
    }

    #[test]
    fn max_concurrent_getter() {
        let tm = ThreadManager::new(42);
        assert_eq!(tm.max_concurrent(), 42);
    }
}
