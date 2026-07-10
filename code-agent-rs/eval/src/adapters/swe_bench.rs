//! SWE-bench-Live benchmark adapter.
//!
//! SWE-bench (Software Engineering Benchmark) evaluates an agent's ability
//! to resolve real-world GitHub issues by producing a correct patch. Each
//! task provides a repository, a base commit, an issue description, and
//! a set of test cases that must pass after the fix.
//!
//! ## Dataset format
//!
//! Each entry in SWE-bench-Live is a JSON object with:
//!
//! ```json
//! {
//!   "instance_id": "django__django-12345",
//!   "repo": "django/django",
//!   "base_commit": "abc123...",
//!   "problem_statement": "The admin page crashes when...",
//!   "hints_text": "",
//!   "patch": "diff --git ...",
//!   "test_patch": "diff --git ...",
//!   "FAIL_TO_PASS": "[\"tests/admin/test_views.py::TestAdmin::test_crash\"]",
//!   "PASS_TO_PASS": "[\"tests/admin/test_views.py::TestAdmin::test_normal\"]",
//!   "version": "5.0"
//! }
//! ```
//!
//! ## Evaluation flow
//!
//! 1. Clone repository at `base_commit`.
//! 2. Apply `test_patch` (sets up test infrastructure).
//! 3. Agent generates a patch.
//! 4. Apply the agent's patch.
//! 5. Run `FAIL_TO_PASS` tests → must all pass.
//! 6. Run `PASS_TO_PASS` tests → must all still pass.
//! 7. Score = (passed FAIL_TO_PASS + passed PASS_TO_PASS) / total tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{AdapterError, AgentOutput, BenchmarkAdapter};
use crate::types::{EvalResult, EvalStatus, EvalTask};

// ---------------------------------------------------------------------------
// SWE-bench-Live dataset types
// ---------------------------------------------------------------------------

/// SWE-bench-Live 任务实例
///
/// 【领域含义】SWE-bench-Live 数据集中单个任务实例的值对象，描述一个真实的 GitHub Issue。
/// 【核心职责】封装仓库信息、问题描述、测试用例和参考补丁，供适配器转换为标准 EvalTask。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SWEBenchInstance {
    /// 实例标识 — 唯一标识符（如 "django__django-12345"）。
    pub instance_id: String,
    /// 仓库 slug — 如 "django/django"。
    pub repo: String,
    /// 基准提交哈希 — 应用补丁前需要 checkout 的提交。
    pub base_commit: String,
    /// 问题描述 — 自然语言描述的 Issue 内容。
    pub problem_statement: String,
    /// 提示文本 — 给 Agent 的可选提示。
    #[serde(default)]
    pub hints_text: String,
    /// 参考补丁 — 标准答案补丁（用于验证，不用于评估）。
    #[serde(default)]
    pub patch: String,
    /// 测试补丁 — 在仓库中设置测试框架的补丁。
    #[serde(default)]
    pub test_patch: String,
    /// FAIL_TO_PASS 测试 — JSON 编码的 pytest 测试标识符数组，当前失败、修复后应通过。
    #[serde(default)]
    #[serde(alias = "FAIL_TO_PASS")]
    pub fail_to_pass: String,
    /// PASS_TO_PASS 测试 — JSON 编码的 pytest 测试标识符数组，当前通过、修复后仍应通过。
    #[serde(default)]
    #[serde(alias = "PASS_TO_PASS")]
    pub pass_to_pass: String,
    /// 软件版本 — 环境设置所需的版本信息。
    #[serde(default)]
    pub version: String,
}

impl SWEBenchInstance {
    /// 解析 FAIL_TO_PASS 测试列表
    ///
    /// 【领域含义】将 JSON 编码的 FAIL_TO_PASS 字符串解析为测试标识符列表。
    /// 【核心职责】提供便捷方法获取当前失败、修复后应通过的测试列表。
    pub fn fail_to_pass_tests(&self) -> Vec<String> {
        parse_test_list(&self.fail_to_pass)
    }

    /// 解析 PASS_TO_PASS 测试列表
    ///
    /// 【领域含义】将 JSON 编码的 PASS_TO_PASS 字符串解析为测试标识符列表。
    /// 【核心职责】提供便捷方法获取当前通过、修复后仍应通过的测试列表。
    pub fn pass_to_pass_tests(&self) -> Vec<String> {
        parse_test_list(&self.pass_to_pass)
    }

    /// 获取测试用例总数
    ///
    /// 【领域含义】计算 FAIL_TO_PASS 和 PASS_TO_PASS 测试用例的总数。
    /// 【核心职责】供评分逻辑计算通过率时使用。
    pub fn total_test_count(&self) -> usize {
        self.fail_to_pass_tests().len() + self.pass_to_pass_tests().len()
    }

    /// 获取仓库克隆 URL
    ///
    /// 【领域含义】根据 repo 字段生成 GitHub 克隆 URL。
    /// 【核心职责】供 setup 命令生成 git clone 指令。
    pub fn clone_url(&self) -> String {
        format!("https://github.com/{}.git", self.repo)
    }
}

/// Parse a JSON-encoded array of test identifiers (e.g. `"[\"a\", \"b\"]"`).
fn parse_test_list(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str(raw).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// SWEBenchLiveAdapter
// ---------------------------------------------------------------------------

/// SWE-bench-Live 适配器
///
/// 【领域含义】SWE-bench-Live 基准测试的适配器实现，将 SWE-bench 原生格式转换为标准评估流程。
/// 【核心职责】加载数据集、生成 EvalTask、评估 Agent 生成的补丁。
///
/// 支持的数据集分片：
/// - `"lite"` — 轻量子集（约 300 个实例）。
/// - `"verified"` — 已验证子集。
/// - `"full"` — 完整数据集（1000+ 实例）。
///
/// ## 使用示例
///
/// ```no_run
/// use std::path::Path;
/// use code_agent_eval::adapters::swe_bench::SWEBenchLiveAdapter;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let adapter = SWEBenchLiveAdapter::new(Path::new("./swe_bench_data"), "lite");
/// let tasks = adapter.load_tasks(Some(10)).await?;
/// # Ok(())
/// # }
/// ```
pub struct SWEBenchLiveAdapter {
    /// 数据集缓存目录
    cache_dir: PathBuf,
    /// 数据集分片名称
    split: String,
    /// 已解析的实例缓存，以 instance_id 为键
    /// 由 load_tasks 填充，供 evaluate_patch 使用
    instances: RwLock<BTreeMap<String, SWEBenchInstance>>,
}

impl SWEBenchLiveAdapter {
    /// 创建适配器
    ///
    /// 【领域含义】构造 SWE-bench-Live 适配器实例。
    /// 【核心职责】指定数据集缓存目录和分片名称。
    ///
    /// * `cache_dir` — 数据集 JSONL 文件的存放/接收目录。
    /// * `split` — 分片名称："lite"、"verified" 或 "full"。
    pub fn new(cache_dir: &Path, split: &str) -> Self {
        Self {
            cache_dir: cache_dir.to_path_buf(),
            split: split.to_string(),
            instances: RwLock::new(BTreeMap::new()),
        }
    }

    /// Path to the JSONL dataset file for our split.
    fn dataset_path(&self) -> PathBuf {
        self.cache_dir
            .join(format!("swe_bench_live_{}.jsonl", self.split))
    }

    /// 解析数据集文件中的所有实例
    ///
    /// 【领域含义】从 JSONL 格式的数据集文件中解析所有 SWE-bench 实例。
    /// 【核心职责】逐行读取 JSONL 文件，反序列化为 SWEBenchInstance。
    pub fn parse_instances(&self) -> Result<Vec<SWEBenchInstance>, AdapterError> {
        let path = self.dataset_path();
        let content = std::fs::read_to_string(&path).map_err(|e| {
            AdapterError::DatasetLoad(format!(
                "Cannot read {}: {e}. Run `download` first or place the file manually.",
                path.display()
            ))
        })?;

        let mut instances = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let instance: SWEBenchInstance = serde_json::from_str(trimmed)
                .map_err(|e| AdapterError::DatasetLoad(format!("Line {}: {e}", i + 1)))?;
            instances.push(instance);
        }
        Ok(instances)
    }

    /// 从 HuggingFace 下载数据集
    ///
    /// 【领域含义】如果本地不存在数据集文件，从 HuggingFace 下载指定分片的 JSONL 文件。
    /// 【核心职责】使用 HuggingFace datasets 服务器 API 获取数据集，需要 `remote-datasets` 特性。
    #[cfg(feature = "remote-datasets")]
    pub async fn download(&self) -> Result<(), AdapterError> {
        let path = self.dataset_path();
        if path.exists() {
            tracing::info!(path = %path.display(), "Dataset already downloaded");
            return Ok(());
        }

        tracing::info!(split = %self.split, "Downloading SWE-bench-Live dataset from HuggingFace");

        let url = format!(
            "https://huggingface.co/datasets/SWE-bench-Live/SWE-bench-Live/resolve/main/data/{split}.jsonl",
            split = self.split
        );

        let response = reqwest::get(&url).await?;
        if !response.status().is_success() {
            return Err(AdapterError::DatasetLoad(format!(
                "HuggingFace returned HTTP {} for {url}",
                response.status()
            )));
        }

        let body = response.text().await?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::write(&path, &body)?;
        tracing::info!(path = %path.display(), bytes = body.len(), "Dataset downloaded");

        Ok(())
    }

    /// 下载存根（remote-datasets 特性未启用时）
    ///
    /// 【领域含义】当 remote-datasets 特性未启用时，检查本地文件是否存在。
    /// 【核心职责】如果文件不存在则返回错误，提示用户手动放置文件或启用特性。
    #[cfg(not(feature = "remote-datasets"))]
    pub async fn download(&self) -> Result<(), AdapterError> {
        let path = self.dataset_path();
        if !path.exists() {
            return Err(AdapterError::DatasetLoad(format!(
                "Dataset file {} not found and `remote-datasets` feature is disabled. \
                 Place the JSONL file manually or enable the feature.",
                path.display()
            )));
        }
        Ok(())
    }

    /// 加载任务实例
    ///
    /// 【领域含义】从数据集加载任务实例，可选限制加载数量。
    /// 【核心职责】下载数据集、解析实例、填充内部缓存、转换为 EvalTask 列表。
    /// 填充内部实例缓存，供后续 evaluate_patch 查找测试数据。
    pub async fn load_tasks(&self, limit: Option<usize>) -> Result<Vec<EvalTask>, AdapterError> {
        // Ensure dataset is available.
        self.download().await?;

        let all_instances = self.parse_instances()?;
        let instances: Vec<_> = match limit {
            Some(n) => all_instances.into_iter().take(n).collect(),
            None => all_instances,
        };

        // Populate cache.
        {
            let mut cache = self.instances.write().expect("instances lock poisoned");
            cache.clear();
            for inst in &instances {
                cache.insert(inst.instance_id.clone(), inst.clone());
            }
        }

        let tasks: Vec<EvalTask> = instances.iter().map(Self::instance_to_task).collect();
        Ok(tasks)
    }

    /// 将 SWEBenchInstance 转换为 EvalTask
    ///
    /// 【领域含义】将 SWE-bench 原生实例转换为标准评估任务。
    /// 【核心职责】生成 setup 命令序列：克隆仓库 → checkout 基准提交 → 应用测试补丁 → 运行 pytest。
    pub fn instance_to_task(instance: &SWEBenchInstance) -> EvalTask {
        let repo_dir = format!("/tmp/{}", instance.instance_id.replace('/', "_"));

        let fail_tests = instance.fail_to_pass_tests();
        let pass_tests = instance.pass_to_pass_tests();
        let all_tests: Vec<String> = fail_tests
            .iter()
            .chain(pass_tests.iter())
            .cloned()
            .collect();

        let setup_commands = vec![
            format!(
                "git clone --depth 1 {} {repo_dir}",
                instance.clone_url()
            ),
            format!("cd {repo_dir} && git fetch --depth 1 origin {}", instance.base_commit),
            format!("cd {repo_dir} && git checkout {}", instance.base_commit),
            format!("echo '{}' | base64 -d | git -C {repo_dir} apply --verbose", {
                // base64-encode the test_patch to avoid shell escaping issues.
                use std::io::Write;
                let mut encoder = base64_write::Base64Writer::new(Vec::new());
                encoder.write_all(instance.test_patch.as_bytes()).ok();
                String::from_utf8(encoder.into_inner()).unwrap_or_default()
            }),
        ];

        let test_command = if all_tests.is_empty() {
            "echo 'NO_TESTS'".to_string()
        } else {
            format!(
                "cd {repo_dir} && python -m pytest {} -x --tb=short 2>&1",
                all_tests.join(" ")
            )
        };

        EvalTask {
            task_id: instance.instance_id.clone(),
            setup_commands,
            test_commands: vec![test_command],
            expected_output: None,
            timeout_secs: 600, // SWE-bench tasks can be large.
            repository: Some(instance.clone_url()),
            base_commit: Some(instance.base_commit.clone()),
        }
    }

    /// 评估生成的补丁
    ///
    /// 【领域含义】SWE-bench 的核心评估逻辑：对 Agent 生成的补丁进行评分。
    /// 【核心职责】从缓存查找实例 → 应用 Agent 补丁 → 运行测试套件 → 统计通过数 → 计算分数。
    pub async fn evaluate_patch(
        &self,
        task: &EvalTask,
        patch: &str,
    ) -> Result<EvalResult, AdapterError> {
        if patch.trim().is_empty() {
            return Ok(EvalResult {
                task_id: task.task_id.clone(),
                status: EvalStatus::Fail,
                score: Some(0.0),
                logs: vec!["Empty patch — nothing to evaluate".into()],
                patch: Some(patch.to_string()),
                metrics: Default::default(),
            });
        }

        let instances = self.instances.read().expect("instances lock poisoned");
        let instance = instances
            .get(&task.task_id)
            .ok_or_else(|| AdapterError::TaskNotFound(task.task_id.clone()))?;

        let repo_dir = format!("/tmp/{}_eval", instance.instance_id.replace('/', "_"));

        // Apply the agent's patch inside the repository.
        let temp_dir = tempfile::tempdir()?;
        let patch_file = temp_dir.path().join("agent.patch");
        std::fs::write(&patch_file, patch)?;

        let apply_result = std::process::Command::new("git")
            .args(["apply", "--check"])
            .arg(&patch_file)
            .current_dir(&repo_dir)
            .output();

        let mut logs: Vec<String> = Vec::new();

        match apply_result {
            Ok(out) if out.status.success() => {
                // Patch applies cleanly — now apply it for real.
                let _ = std::process::Command::new("git")
                    .args(["apply"])
                    .arg(&patch_file)
                    .current_dir(&repo_dir)
                    .output();

                logs.push("Patch applied successfully".into());
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                logs.push(format!("Patch does not apply cleanly: {stderr}"));
                return Ok(EvalResult {
                    task_id: task.task_id.clone(),
                    status: EvalStatus::Fail,
                    score: Some(0.0),
                    logs,
                    patch: Some(patch.to_string()),
                    metrics: Default::default(),
                });
            }
            Err(e) => {
                logs.push(format!("Failed to run git apply: {e}"));
                return Ok(EvalResult {
                    task_id: task.task_id.clone(),
                    status: EvalStatus::Error,
                    score: Some(0.0),
                    logs,
                    patch: Some(patch.to_string()),
                    metrics: Default::default(),
                });
            }
        }

        // Run the test suite: FAIL_TO_PASS + PASS_TO_PASS.
        let fail_tests = instance.fail_to_pass_tests();
        let pass_tests = instance.pass_to_pass_tests();

        let total = fail_tests.len() + pass_tests.len();
        if total == 0 {
            return Ok(EvalResult {
                task_id: task.task_id.clone(),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs,
                patch: Some(patch.to_string()),
                metrics: Default::default(),
            });
        }

        let mut passed = 0usize;

        // Run FAIL_TO_PASS tests.
        for test_id in &fail_tests {
            let output = std::process::Command::new("python")
                .args(["-m", "pytest", test_id, "-x", "--tb=short"])
                .current_dir(&repo_dir)
                .output();

            match output {
                Ok(out) => {
                    if out.status.success() {
                        passed += 1;
                        logs.push(format!("PASS: {test_id}"));
                    } else {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        logs.push(format!("FAIL (should pass): {test_id} — {stderr}"));
                    }
                }
                Err(e) => {
                    logs.push(format!("ERROR running {test_id}: {e}"));
                }
            }
        }

        // Run PASS_TO_PASS tests.
        for test_id in &pass_tests {
            let output = std::process::Command::new("python")
                .args(["-m", "pytest", test_id, "-x", "--tb=short"])
                .current_dir(&repo_dir)
                .output();

            match output {
                Ok(out) => {
                    if out.status.success() {
                        passed += 1;
                        logs.push(format!("PASS: {test_id}"));
                    } else {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        logs.push(format!("FAIL (should pass): {test_id} — {stderr}"));
                    }
                }
                Err(e) => {
                    logs.push(format!("ERROR running {test_id}: {e}"));
                }
            }
        }

        let score = passed as f64 / total as f64;
        let status = if (score - 1.0).abs() < 1e-9 {
            EvalStatus::Pass
        } else {
            EvalStatus::Fail
        };

        Ok(EvalResult {
            task_id: task.task_id.clone(),
            status,
            score: Some(score),
            logs,
            patch: Some(patch.to_string()),
            metrics: Default::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// BenchmarkAdapter impl
// ---------------------------------------------------------------------------

#[async_trait]
impl BenchmarkAdapter for SWEBenchLiveAdapter {
    fn name(&self) -> &str {
        "swe_bench_live"
    }

    async fn load_tasks(&self) -> Result<Vec<EvalTask>, AdapterError> {
        self.load_tasks(None).await
    }

    async fn process_result(
        &self,
        task: &EvalTask,
        output: &AgentOutput,
    ) -> Result<EvalResult, AdapterError> {
        // Extract patch from the agent output.
        let patch = output
            .patch
            .as_ref()
            .or(if !output.content.is_empty() {
                Some(&output.content)
            } else {
                None
            })
            .ok_or_else(|| {
                AdapterError::Parse("No patch found in agent output".into())
            })?;

        self.evaluate_patch(task, patch).await
    }
}

// ---------------------------------------------------------------------------
// base64 helper (lightweight, no external crate needed)
// ---------------------------------------------------------------------------

mod base64_write {
    use std::io::{self, Write};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub struct Base64Writer<W: Write> {
        inner: W,
        buf: u32,
        buf_len: u8,
    }

    impl<W: Write> Base64Writer<W> {
        pub fn new(inner: W) -> Self {
            Self {
                inner,
                buf: 0,
                buf_len: 0,
            }
        }

        pub fn into_inner(self) -> W {
            self.inner
        }
    }

    impl<W: Write> Write for Base64Writer<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            for &byte in data {
                self.buf = (self.buf << 8) | byte as u32;
                self.buf_len += 8;
                while self.buf_len >= 6 {
                    self.buf_len -= 6;
                    let idx = ((self.buf >> self.buf_len) & 0x3F) as usize;
                    let out = [ALPHABET[idx]];
                    self.inner.write_all(&out)?;
                }
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.buf_len > 0 {
                let idx = ((self.buf << (6 - self.buf_len)) & 0x3F) as usize;
                let out = [ALPHABET[idx]];
                self.inner.write_all(&out)?;
                // Padding
                while self.buf_len > 0 {
                    self.buf_len = self.buf_len.saturating_sub(6);
                    if self.buf_len < 6 {
                        self.inner.write_all(b"=")?;
                    }
                }
            }
            self.inner.flush()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EvalStatus;

    /// A minimal SWE-bench-Live instance fixture.
    const FIXTURE_JSONL: &str = r#"{"instance_id":"test__repo-0","repo":"test/repo","base_commit":"deadbeef","problem_statement":"Fix the bug","hints_text":"","patch":"","test_patch":"diff --git a/test.py b/test.py","FAIL_TO_PASS":"[\"tests/test_a.py::test_1\"]","PASS_TO_PASS":"[\"tests/test_b.py::test_2\"]","version":"1.0"}
"#;

    #[test]
    fn parse_single_instance_from_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        let instances = adapter.parse_instances().unwrap();

        assert_eq!(instances.len(), 1);
        let inst = &instances[0];
        assert_eq!(inst.instance_id, "test__repo-0");
        assert_eq!(inst.repo, "test/repo");
        assert_eq!(inst.base_commit, "deadbeef");
        assert_eq!(inst.problem_statement, "Fix the bug");
        assert_eq!(inst.fail_to_pass_tests(), vec!["tests/test_a.py::test_1"]);
        assert_eq!(inst.pass_to_pass_tests(), vec!["tests/test_b.py::test_2"]);
        assert_eq!(inst.total_test_count(), 2);
    }

    #[test]
    fn parse_instance_empty_test_lists() {
        let json = r#"{"instance_id":"x","repo":"a/b","base_commit":"abc","problem_statement":"","patch":"","test_patch":"","FAIL_TO_PASS":"","PASS_TO_PASS":"","version":""}"#;
        let inst: SWEBenchInstance = serde_json::from_str(json).unwrap();
        assert!(inst.fail_to_pass_tests().is_empty());
        assert!(inst.pass_to_pass_tests().is_empty());
        assert_eq!(inst.total_test_count(), 0);
    }

    #[test]
    fn parse_instance_invalid_json_test_list() {
        let inst = SWEBenchInstance {
            instance_id: "id".into(),
            repo: "r".into(),
            base_commit: "b".into(),
            problem_statement: "p".into(),
            hints_text: String::new(),
            patch: String::new(),
            test_patch: String::new(),
            fail_to_pass: "not json".into(),
            pass_to_pass: "[\"ok\"]".into(),
            version: String::new(),
        };
        assert!(inst.fail_to_pass_tests().is_empty());
        assert_eq!(inst.pass_to_pass_tests(), vec!["ok"]);
    }

    #[test]
    fn instance_clone_url() {
        let inst = SWEBenchInstance {
            instance_id: "id".into(),
            repo: "org/repo".into(),
            base_commit: "abc".into(),
            problem_statement: String::new(),
            hints_text: String::new(),
            patch: String::new(),
            test_patch: String::new(),
            fail_to_pass: String::new(),
            pass_to_pass: String::new(),
            version: String::new(),
        };
        assert_eq!(inst.clone_url(), "https://github.com/org/repo.git");
    }

    #[test]
    fn instance_to_task_generates_setup_commands() {
        let instance = SWEBenchInstance {
            instance_id: "test__repo-0".into(),
            repo: "test/repo".into(),
            base_commit: "abc123".into(),
            problem_statement: "problem".into(),
            hints_text: String::new(),
            patch: String::new(),
            test_patch: "test diff content".into(),
            fail_to_pass: "[\"t1\"]".into(),
            pass_to_pass: "[\"t2\", \"t3\"]".into(),
            version: "1.0".into(),
        };

        let task = SWEBenchLiveAdapter::instance_to_task(&instance);

        assert_eq!(task.task_id, "test__repo-0");
        assert_eq!(task.timeout_secs, 600);
        assert_eq!(task.repository, Some("https://github.com/test/repo.git".into()));
        assert_eq!(task.base_commit, Some("abc123".into()));

        // Setup commands should include git clone/fetch/checkout and test_patch.
        assert!(task.setup_commands.len() >= 3);
        assert!(task.setup_commands[0].contains("git clone"));
        assert!(task.setup_commands[2].contains("git checkout abc123"));

        // Test command should include pytest with the test identifiers.
        assert_eq!(task.test_commands.len(), 1);
        assert!(task.test_commands[0].contains("pytest"));
        assert!(task.test_commands[0].contains("t1"));
        assert!(task.test_commands[0].contains("t2"));
        assert!(task.test_commands[0].contains("t3"));
    }

    #[test]
    fn instance_to_task_no_tests() {
        let instance = SWEBenchInstance {
            instance_id: "no_tests".into(),
            repo: "a/b".into(),
            base_commit: "abc".into(),
            problem_statement: String::new(),
            hints_text: String::new(),
            patch: String::new(),
            test_patch: String::new(),
            fail_to_pass: String::new(),
            pass_to_pass: String::new(),
            version: String::new(),
        };

        let task = SWEBenchLiveAdapter::instance_to_task(&instance);
        assert_eq!(task.test_commands, vec!["echo 'NO_TESTS'"]);
    }

    #[test]
    fn parse_multiple_instances() {
        let jsonl = format!("{FIXTURE_JSONL}\n{FIXTURE_JSONL}");
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, jsonl).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        let instances = adapter.parse_instances().unwrap();
        assert_eq!(instances.len(), 2);
    }

    #[test]
    fn adapter_name() {
        let adapter = SWEBenchLiveAdapter::new(Path::new("/tmp"), "lite");
        assert_eq!(adapter.name(), "swe_bench_live");
    }

    // ------------------------------------------------------------------
    // evaluate_patch unit tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn evaluate_patch_empty_returns_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        adapter.load_tasks(Some(1)).await.unwrap();

        let task = EvalTask {
            task_id: "test__repo-0".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };

        let result = adapter.evaluate_patch(&task, "").await.unwrap();
        assert_eq!(result.status, EvalStatus::Fail);
        assert!(result.logs.iter().any(|l| l.contains("Empty patch")));
    }

    #[tokio::test]
    async fn evaluate_patch_task_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        adapter.load_tasks(Some(1)).await.unwrap();

        let task = EvalTask {
            task_id: "missing__task".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };

        let result = adapter.evaluate_patch(&task, "some patch").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Task not found"));
    }

    // ------------------------------------------------------------------
    // process_result (adapter trait) tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn process_result_no_patch_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        adapter.load_tasks(Some(1)).await.unwrap();

        let task = EvalTask {
            task_id: "test__repo-0".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };
        let output = AgentOutput::default(); // empty — no patch

        let result = adapter.process_result(&task, &output).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No patch"));
    }

    #[tokio::test]
    async fn process_result_uses_patch_from_content_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl_path = tmp.path().join("swe_bench_live_lite.jsonl");
        std::fs::write(&jsonl_path, FIXTURE_JSONL).unwrap();

        let adapter = SWEBenchLiveAdapter::new(tmp.path(), "lite");
        adapter.load_tasks(Some(1)).await.unwrap();

        let task = EvalTask {
            task_id: "test__repo-0".into(),
            test_commands: vec!["echo ok".into()],
            ..Default::default()
        };
        // content is non-empty but patch field is None — should fall back to content.
        let output = AgentOutput {
            content: "diff --git a/x b/x".into(),
            patch: None,
            ..Default::default()
        };

        // This will try to evaluate the patch against a repo that doesn't exist,
        // but the code path that extracts the patch from content is what we test.
        let result = adapter.process_result(&task, &output).await;
        // It will fail because there's no repo, but it should NOT be a "No patch" error.
        match result {
            Ok(r) => {
                // Might succeed if the repo happens to exist, but unlikely.
                assert!(!r.logs.is_empty());
            }
            Err(e) => {
                // Should NOT be "No patch found"
                assert!(
                    !e.to_string().contains("No patch"),
                    "should have found patch in content, got: {}",
                    e
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // base64 writer
    // ------------------------------------------------------------------

    #[test]
    fn base64_encode_simple() {
        let mut w = base64_write::Base64Writer::new(Vec::new());
        use std::io::Write;
        w.write_all(b"hello").unwrap();
        w.flush().unwrap();
        let encoded = String::from_utf8(w.into_inner()).unwrap();
        // "hello" → "aGVsbG8="
        assert_eq!(encoded, "aGVsbG8=");
    }

    #[test]
    fn base64_encode_empty() {
        let mut w = base64_write::Base64Writer::new(Vec::new());
        use std::io::Write;
        w.flush().unwrap();
        let encoded = String::from_utf8(w.into_inner()).unwrap();
        assert!(encoded.is_empty());
    }

    #[test]
    fn base64_roundtrip_via_decode() {
        let input = b"The quick brown fox jumps over the lazy dog";
        let mut w = base64_write::Base64Writer::new(Vec::new());
        use std::io::Write;
        w.write_all(input).unwrap();
        w.flush().unwrap();
        let encoded = String::from_utf8(w.into_inner()).unwrap();

        // Decode using standard base64.
        use std::io::Read;
        let mut decoder = base64_read::Base64Reader::new(encoded.as_bytes());
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();

        assert_eq!(decoded, input);
    }

    // Minimal base64 decoder for roundtrip testing.
    mod base64_read {
        use std::io::{self, Read};

        pub struct Base64Reader<R: Read> {
            inner: R,
            buf: u32,
            buf_len: u8,
            done: bool,
        }

        impl<R: Read> Base64Reader<R> {
            pub fn new(inner: R) -> Self {
                Self {
                    inner,
                    buf: 0,
                    buf_len: 0,
                    done: false,
                }
            }
        }

        impl<R: Read> Read for Base64Reader<R> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.done && self.buf_len == 0 {
                    return Ok(0);
                }

                let mut written = 0usize;
                let mut tmp = [0u8; 1];

                for slot in out.iter_mut() {
                    // Fill buffer until we have 8 bits.
                    while self.buf_len < 8 && !self.done {
                        match self.inner.read(&mut tmp)? {
                            0 => {
                                self.done = true;
                                break;
                            }
                            1 => {
                                let c = tmp[0];
                                let val = match c {
                                    b'A'..=b'Z' => c - b'A',
                                    b'a'..=b'z' => c - b'a' + 26,
                                    b'0'..=b'9' => c - b'0' + 52,
                                    b'+' => 62,
                                    b'/' => 63,
                                    b'=' => {
                                        self.done = true;
                                        break;
                                    }
                                    b'\n' | b'\r' | b' ' => continue,
                                    _ => {
                                        return Err(io::Error::new(
                                            io::ErrorKind::InvalidData,
                                            format!("Invalid base64 char: {c}"),
                                        ));
                                    }
                                };
                                self.buf = (self.buf << 6) | val as u32;
                                self.buf_len += 6;
                            }
                            _ => unreachable!(),
                        }
                    }

                    if self.buf_len >= 8 {
                        self.buf_len -= 8;
                        *slot = (self.buf >> self.buf_len) as u8;
                        written += 1;
                    } else {
                        break;
                    }
                }

                if written > 0 {
                    return Ok(written);
                }
                Ok(0)
            }
        }
    }
}
