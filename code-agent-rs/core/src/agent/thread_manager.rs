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

use code_agent_protocol::{SharePermission, ThreadId};

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
    /// 按 ThreadId 索引的共享访问列表：被共享用户 → 权限级别
    shared_access: HashMap<ThreadId, Vec<(String, SharePermission)>>,
    /// 按 ThreadId 索引的团队共享列表：团队 ID → 权限级别
    team_shares: HashMap<ThreadId, Vec<(String, SharePermission)>>,
    /// 最大并发线程数限制
    max_concurrent: usize,
}

impl ThreadManager {
    /// 创建新的线程管理器，指定最大并发线程数
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            threads: HashMap::new(),
            user_map: HashMap::new(),
            shared_access: HashMap::new(),
            team_shares: HashMap::new(),
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
        self.shared_access.remove(id);
        self.team_shares.remove(id);
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

    /// 将线程共享给其他用户。
    ///
    /// 【领域行为】线程所有者可以将线程共享给其他用户，并指定权限级别。
    /// 如果目标用户已在共享列表中，则更新其权限。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者，返回错误。
    pub fn share_thread(
        &mut self,
        thread_id: ThreadId,
        owner_id: &str,
        target_user: String,
        permission: SharePermission,
    ) -> Result<(), String> {
        if !self.check_ownership(&thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        let entry = self.shared_access.entry(thread_id).or_default();
        if let Some(existing) = entry.iter_mut().find(|(u, _)| u == &target_user) {
            existing.1 = permission;
        } else {
            entry.push((target_user, permission));
        }
        Ok(())
    }

    /// 撤销线程共享。
    ///
    /// 【领域行为】线程所有者可以撤销之前对某个用户的共享。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者或目标用户不在共享列表中，返回错误。
    pub fn unshare_thread(
        &mut self,
        thread_id: ThreadId,
        owner_id: &str,
        target_user: &str,
    ) -> Result<(), String> {
        if !self.check_ownership(&thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        let entry = self
            .shared_access
            .get_mut(&thread_id)
            .ok_or_else(|| format!("no shared users for thread '{}'", thread_id))?;
        let orig_len = entry.len();
        entry.retain(|(u, _)| u != target_user);
        if entry.len() == orig_len {
            return Err(format!(
                "user '{}' not found in shared list for thread '{}'",
                target_user, thread_id
            ));
        }
        if entry.is_empty() {
            self.shared_access.remove(&thread_id);
        }
        Ok(())
    }

    /// 检查用户是否有权访问线程（所有者 或 被共享者）。
    pub fn check_access(&self, thread_id: &ThreadId, user_id: &str) -> bool {
        self.check_ownership(thread_id, user_id)
            || self
                .shared_access
                .get(thread_id)
                .map(|v| v.iter().any(|(u, _)| u == user_id))
                .unwrap_or(false)
    }

    /// 列出指定用户被共享访问的所有线程 ID。
    pub fn list_shared_threads(&self, user_id: &str) -> Vec<ThreadId> {
        self.shared_access
            .iter()
            .filter(|(_, v)| v.iter().any(|(u, _)| u == user_id))
            .map(|(tid, _)| tid.clone())
            .collect()
    }

    /// 获取线程的共享用户列表（仅所有者可调用）。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者，返回错误。
    /// 如果线程存在但没有共享用户，也返回错误。
    pub fn get_shared_users(
        &self,
        thread_id: &ThreadId,
        owner_id: &str,
    ) -> Result<&[(String, SharePermission)], String> {
        if !self.check_ownership(thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        self.shared_access
            .get(thread_id)
            .map(|v| v.as_slice())
            .ok_or_else(|| format!("thread '{}' has no shared users", thread_id))
    }

    /// 将线程共享给整个团队。
    ///
    /// 【领域行为】线程所有者可以将线程共享给一个团队，团队所有成员都能访问。
    /// 如果该团队已在共享列表中，则更新其权限。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者，返回错误。
    pub fn share_thread_with_team(
        &mut self,
        thread_id: ThreadId,
        owner_id: &str,
        team_id: String,
        permission: SharePermission,
    ) -> Result<(), String> {
        if !self.check_ownership(&thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        let entry = self.team_shares.entry(thread_id).or_default();
        if let Some(existing) = entry.iter_mut().find(|(u, _)| u == &team_id) {
            existing.1 = permission;
        } else {
            entry.push((team_id, permission));
        }
        Ok(())
    }

    /// 撤销线程对团队的共享。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者或该团队不在共享列表中，返回错误。
    pub fn unshare_thread_with_team(
        &mut self,
        thread_id: ThreadId,
        owner_id: &str,
        team_id: &str,
    ) -> Result<(), String> {
        if !self.check_ownership(&thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        let entry = self
            .team_shares
            .get_mut(&thread_id)
            .ok_or_else(|| format!("no team shares for thread '{}'", thread_id))?;
        let orig_len = entry.len();
        entry.retain(|(u, _)| u != team_id);
        if entry.len() == orig_len {
            return Err(format!(
                "team '{}' not found in shared list for thread '{}'",
                team_id, thread_id
            ));
        }
        if entry.is_empty() {
            self.team_shares.remove(&thread_id);
        }
        Ok(())
    }

    /// 列出指定团队可访问的线程 ID。
    pub fn list_team_shared_threads(&self, team_id: &str) -> Vec<ThreadId> {
        self.team_shares
            .iter()
            .filter(|(_, v)| v.iter().any(|(t, _)| t == team_id))
            .map(|(tid, _)| tid.clone())
            .collect()
    }

    /// 检查用户的任何团队是否有权访问线程。
    ///
    /// 【领域行为】传入用户所属的团队列表，检查是否有任一团队被共享给该线程。
    /// 配合 `check_access()` 使用可实现完整访问控制：
    /// `check_access(thread_id, user_id) || check_team_access(thread_id, user_teams)`
    pub fn check_team_access(&self, thread_id: &ThreadId, user_teams: &[String]) -> bool {
        self.team_shares
            .get(thread_id)
            .map(|teams| teams.iter().any(|(t, _)| user_teams.iter().any(|ut| ut == t)))
            .unwrap_or(false)
    }

    /// 获取线程的团队共享列表（仅所有者可调用）。
    ///
    /// 【领域行为】与 `get_shared_users` 对称，线程所有者可以查看哪些团队被共享。
    ///
    /// # Errors
    ///
    /// 如果调用者不是线程所有者，返回错误。
    /// 如果线程存在但没有团队共享，也返回错误。
    pub fn get_team_shares_for_thread(
        &self,
        thread_id: &ThreadId,
        owner_id: &str,
    ) -> Result<&[(String, SharePermission)], String> {
        if !self.check_ownership(thread_id, owner_id) {
            return Err(format!(
                "user '{}' does not own thread '{}'",
                owner_id, thread_id
            ));
        }
        self.team_shares
            .get(thread_id)
            .map(|v| v.as_slice())
            .ok_or_else(|| format!("thread '{}' has no team shares", thread_id))
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
            tool_router: None,
            hook_registry: None,
            external_cancel: None,
            max_context_tokens: None,
            temperature: None,
            knowledge: None,
            quality_gate: None,
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

    // -----------------------------------------------------------------------
    // Thread sharing tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn share_thread_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Alice shares with Bob
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Edit,
        )
        .unwrap();

        // Bob now has access
        assert!(tm.check_access(&ThreadId::from("t1"), "bob"));
    }

    #[tokio::test]
    async fn share_thread_verify_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Charlie (not owner) tries to share
        let result = tm.share_thread(
            ThreadId::from("t1"),
            "charlie",
            "bob".into(),
            SharePermission::Read,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn share_thread_update_existing() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share with Read first
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();

        // Update to Admin
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Admin,
        )
        .unwrap();

        let users = tm
            .get_shared_users(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].0, "bob");
        assert_eq!(users[0].1, SharePermission::Admin);
    }

    #[tokio::test]
    async fn unshare_thread_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Edit,
        )
        .unwrap();

        // Unshare
        tm.unshare_thread(ThreadId::from("t1"), "alice", "bob")
            .unwrap();

        // Bob no longer has access
        assert!(!tm.check_access(&ThreadId::from("t1"), "bob"));
        // But alice still does (owner)
        assert!(tm.check_access(&ThreadId::from("t1"), "alice"));
    }

    #[tokio::test]
    async fn unshare_thread_wrong_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();

        let result = tm.unshare_thread(ThreadId::from("t1"), "charlie", "bob");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn unshare_thread_user_not_found() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // No shares at all
        let result = tm.unshare_thread(ThreadId::from("t1"), "alice", "bob");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn check_access_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        assert!(tm.check_access(&ThreadId::from("t1"), "alice"));
        assert!(!tm.check_access(&ThreadId::from("t1"), "bob"));
    }

    #[tokio::test]
    async fn check_access_shared_user() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share with multiple users
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "charlie".into(),
            SharePermission::Edit,
        )
        .unwrap();

        assert!(tm.check_access(&ThreadId::from("t1"), "alice"));
        assert!(tm.check_access(&ThreadId::from("t1"), "bob"));
        assert!(tm.check_access(&ThreadId::from("t1"), "charlie"));
        assert!(!tm.check_access(&ThreadId::from("t1"), "dave"));
    }

    #[tokio::test]
    async fn list_shared_threads_multiple() {
        let mut tm = ThreadManager::new(10);
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
        tm.create_thread(
            ThreadId::from("t3"),
            Session::new(make_config("sess-3")).await,
            "alice".into(),
        )
        .unwrap();

        // Share t1 and t3 with bob, but not t2
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();
        tm.share_thread(
            ThreadId::from("t3"),
            "alice",
            "bob".into(),
            SharePermission::Edit,
        )
        .unwrap();

        let bob_threads = tm.list_shared_threads("bob");
        assert_eq!(bob_threads.len(), 2);
        assert!(bob_threads.contains(&ThreadId::from("t1")));
        assert!(bob_threads.contains(&ThreadId::from("t3")));
        assert!(!bob_threads.contains(&ThreadId::from("t2")));
    }

    #[tokio::test]
    async fn list_shared_threads_none() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        assert!(tm.list_shared_threads("bob").is_empty());
    }

    #[tokio::test]
    async fn get_shared_users_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "charlie".into(),
            SharePermission::Edit,
        )
        .unwrap();

        let users = tm
            .get_shared_users(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|(u, p)| u == "bob" && *p == SharePermission::Read));
        assert!(users.iter().any(|(u, p)| u == "charlie" && *p == SharePermission::Edit));
    }

    #[tokio::test]
    async fn get_shared_users_wrong_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();

        let result = tm.get_shared_users(&ThreadId::from("t1"), "bob");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn get_shared_users_no_shares() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        let result = tm.get_shared_users(&ThreadId::from("t1"), "alice");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remove_thread_cleans_up_shared_access() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Edit,
        )
        .unwrap();

        tm.remove_thread(&ThreadId::from("t1"));
        // After removal, bob should not have access (thread gone)
        assert!(tm.list_shared_threads("bob").is_empty());
    }

    #[tokio::test]
    async fn share_then_unshare_then_recheck() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();
        assert!(tm.check_access(&ThreadId::from("t1"), "bob"));

        // Unshare
        tm.unshare_thread(ThreadId::from("t1"), "alice", "bob")
            .unwrap();
        assert!(!tm.check_access(&ThreadId::from("t1"), "bob"));

        // Re-share with different permission
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Admin,
        )
        .unwrap();
        assert!(tm.check_access(&ThreadId::from("t1"), "bob"));
        let users = tm
            .get_shared_users(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(users[0].1, SharePermission::Admin);
    }

    // -----------------------------------------------------------------------
    // Team sharing tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn share_thread_with_team_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Alice shares thread with team "engineering"
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Edit,
        )
        .unwrap();

        // Check team access: user "bob" in team "engineering" has access
        let bob_teams = vec!["engineering".to_string()];
        assert!(tm.check_team_access(&ThreadId::from("t1"), &bob_teams));
    }

    #[tokio::test]
    async fn share_thread_with_team_verify_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Charlie (not owner) tries to share with team
        let result = tm.share_thread_with_team(
            ThreadId::from("t1"),
            "charlie",
            "engineering".into(),
            SharePermission::Read,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn share_thread_with_team_update_existing() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share with Read first
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();

        // Update to Admin
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Admin,
        )
        .unwrap();

        let teams = tm
            .get_team_shares_for_thread(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(teams.len(), 1);
        assert_eq!(teams[0].0, "engineering");
        assert_eq!(teams[0].1, SharePermission::Admin);
    }

    #[tokio::test]
    async fn unshare_thread_with_team_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Edit,
        )
        .unwrap();

        let bob_teams = vec!["engineering".to_string()];
        assert!(tm.check_team_access(&ThreadId::from("t1"), &bob_teams));

        // Unshare team
        tm.unshare_thread_with_team(ThreadId::from("t1"), "alice", "engineering")
            .unwrap();

        // Team no longer has access
        assert!(!tm.check_team_access(&ThreadId::from("t1"), &bob_teams));
        // But alice still does (owner)
        assert!(tm.check_access(&ThreadId::from("t1"), "alice"));
    }

    #[tokio::test]
    async fn unshare_thread_with_team_wrong_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();

        let result =
            tm.unshare_thread_with_team(ThreadId::from("t1"), "charlie", "engineering");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn unshare_thread_with_team_not_found() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // No team shares
        let result = tm.unshare_thread_with_team(ThreadId::from("t1"), "alice", "engineering");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn check_team_access_multiple_teams() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share with engineering team
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();
        // Share with design team
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "design".into(),
            SharePermission::Edit,
        )
        .unwrap();

        // User in engineering + ops — has access via engineering
        let bob_teams = vec!["engineering".to_string(), "ops".to_string()];
        assert!(tm.check_team_access(&ThreadId::from("t1"), &bob_teams));

        // User in design — has access via design
        let carol_teams = vec!["design".to_string()];
        assert!(tm.check_team_access(&ThreadId::from("t1"), &carol_teams));

        // User in "ops" only — no access
        let dave_teams = vec!["ops".to_string()];
        assert!(!tm.check_team_access(&ThreadId::from("t1"), &dave_teams));

        // User with no teams — no access
        let no_teams: Vec<String> = vec![];
        assert!(!tm.check_team_access(&ThreadId::from("t1"), &no_teams));
    }

    #[tokio::test]
    async fn check_access_individual_and_team_combined() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        // Share individually with bob
        tm.share_thread(
            ThreadId::from("t1"),
            "alice",
            "bob".into(),
            SharePermission::Read,
        )
        .unwrap();
        // Share with engineering team
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Edit,
        )
        .unwrap();

        // Bob: individual access
        assert!(tm.check_access(&ThreadId::from("t1"), "bob"));
        // Carol: team access via engineering
        let carol_teams = vec!["engineering".to_string()];
        assert!(tm.check_team_access(&ThreadId::from("t1"), &carol_teams));
        // Dave: neither individual nor team
        assert!(!tm.check_access(&ThreadId::from("t1"), "dave"));
        let dave_teams = vec!["ops".to_string()];
        assert!(!tm.check_team_access(&ThreadId::from("t1"), &dave_teams));
    }

    #[tokio::test]
    async fn list_team_shared_threads_multiple() {
        let mut tm = ThreadManager::new(10);
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
        tm.create_thread(
            ThreadId::from("t3"),
            Session::new(make_config("sess-3")).await,
            "alice".into(),
        )
        .unwrap();

        // Share t1 and t3 with engineering, but not t2
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();
        tm.share_thread_with_team(
            ThreadId::from("t3"),
            "alice",
            "engineering".into(),
            SharePermission::Edit,
        )
        .unwrap();

        let eng_threads = tm.list_team_shared_threads("engineering");
        assert_eq!(eng_threads.len(), 2);
        assert!(eng_threads.contains(&ThreadId::from("t1")));
        assert!(eng_threads.contains(&ThreadId::from("t3")));
        assert!(!eng_threads.contains(&ThreadId::from("t2")));
    }

    #[tokio::test]
    async fn list_team_shared_threads_none() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        assert!(tm.list_team_shared_threads("engineering").is_empty());
    }

    #[tokio::test]
    async fn get_team_shares_for_thread_success() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "design".into(),
            SharePermission::Edit,
        )
        .unwrap();

        let teams = tm
            .get_team_shares_for_thread(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(teams.len(), 2);
        assert!(teams.iter().any(|(t, p)| t == "engineering" && *p == SharePermission::Read));
        assert!(teams.iter().any(|(t, p)| t == "design" && *p == SharePermission::Edit));
    }

    #[tokio::test]
    async fn get_team_shares_for_thread_wrong_owner() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();

        let result = tm.get_team_shares_for_thread(&ThreadId::from("t1"), "bob");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not own"));
    }

    #[tokio::test]
    async fn get_team_shares_for_thread_no_shares() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        let result = tm.get_team_shares_for_thread(&ThreadId::from("t1"), "alice");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remove_thread_cleans_up_team_shares() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Edit,
        )
        .unwrap();

        tm.remove_thread(&ThreadId::from("t1"));
        // After removal, team should not see thread
        assert!(tm.list_team_shared_threads("engineering").is_empty());
    }

    #[tokio::test]
    async fn team_share_then_unshare_then_recheck() {
        let mut tm = ThreadManager::new(10);
        tm.create_thread(
            ThreadId::from("t1"),
            Session::new(make_config("sess-1")).await,
            "alice".into(),
        )
        .unwrap();

        let bob_teams = vec!["engineering".to_string()];

        // Share with team
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Read,
        )
        .unwrap();
        assert!(tm.check_team_access(&ThreadId::from("t1"), &bob_teams));

        // Unshare team
        tm.unshare_thread_with_team(ThreadId::from("t1"), "alice", "engineering")
            .unwrap();
        assert!(!tm.check_team_access(&ThreadId::from("t1"), &bob_teams));

        // Re-share with different permission
        tm.share_thread_with_team(
            ThreadId::from("t1"),
            "alice",
            "engineering".into(),
            SharePermission::Admin,
        )
        .unwrap();
        assert!(tm.check_team_access(&ThreadId::from("t1"), &bob_teams));
        let teams = tm
            .get_team_shares_for_thread(&ThreadId::from("t1"), "alice")
            .unwrap();
        assert_eq!(teams[0].1, SharePermission::Admin);
    }
}
