//! v2 架构集成测试套件
//!
//! 验证 Plan 引擎、Flywheel、Hook 系统三大 v2 模块的端到端协作。
//! 与 `e2e.rs` 互补——e2e.rs 测试核心 ReAct 循环，本文件测试
//! v2 新增的规划层、数据飞轮和工具钩子体系。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use code_agent_core::agent::plan::{
    PlanNode, TaskPlan, PlanConfig, ExecutionReport, NodeSummary,
    kahn::{InDegreeTracker, parallel_batches},
    quality::{GateVerdict, NoopGate, QualityGate},
    store::{PlanExecutionRecord, PlanNodeRecord, PlanStore},
    decomposition::ManualDecomposer,
    knowledge::KnowledgeGateConfig,
};
use code_agent_core::agent::sub_agent::{AgentResult, AgentStatus};
use code_agent_core::{ErrorTrace, FlywheelCollector};
use code_agent_core::tools::hook::{HookRegistry, NoopHook, ToolEvent, ToolHook, ModelCallSnapshot};

// ===========================================================================
// 1. Plan 引擎集成测试
// ===========================================================================

#[test]
fn test_manual_decomposer_build() {
    let mut decomposer = ManualDecomposer::new("test-plan", vec!["Phase 0", "Phase 1"]);
    decomposer
        .add_task(PlanNode::new("setup", "Setup project", "Phase 0", vec![]))
        .add_task(PlanNode::new("impl", "Implement core", "Phase 1", vec!["setup".to_string()]))
        .add_task(PlanNode::new("test", "Write tests", "Phase 1", vec!["impl".to_string()]));
}

#[test]
fn test_kahn_topological_sort_chain() {
    let mut plan = TaskPlan::new("chain");
    plan.add_node(PlanNode::new("A", "Node A", "Phase 0", vec![]));
    plan.add_node(PlanNode::new("B", "Node B", "Phase 1", vec!["A".to_string()]));
    plan.add_node(PlanNode::new("C", "Node C", "Phase 2", vec!["B".to_string()]));
    plan.add_phase("Phase 0");
    plan.add_phase("Phase 1");
    plan.add_phase("Phase 2");

    let batches = parallel_batches(&plan);
    assert_eq!(batches.len(), 3, "Chain should produce 3 batches");
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[0][0].id, "A");
    assert_eq!(batches[1][0].id, "B");
    assert_eq!(batches[2][0].id, "C");
}

#[test]
fn test_kahn_topological_sort_diamond() {
    let mut plan = TaskPlan::new("diamond");
    plan.add_node(PlanNode::new("A", "Design API", "Phase 0", vec![]));
    plan.add_node(PlanNode::new("B", "Design DB", "Phase 0", vec![]));
    plan.add_node(PlanNode::new("C", "Impl API", "Phase 1", vec!["A".to_string()]));
    plan.add_node(PlanNode::new("D", "Impl DB", "Phase 1", vec!["B".to_string()]));
    plan.add_node(PlanNode::new("E", "Integration", "Phase 2", vec!["C".to_string(), "D".to_string()]));

    let batches = parallel_batches(&plan);
    assert_eq!(batches.len(), 3);
    assert_eq!(batches[0].len(), 2);
    assert_eq!(batches[1].len(), 2);
    assert_eq!(batches[2].len(), 1);
}

#[test]
fn test_kahn_empty_and_single_node() {
    let empty = TaskPlan::new("empty");
    assert!(parallel_batches(&empty).is_empty());

    let mut single = TaskPlan::new("single");
    single.add_node(PlanNode::new("only", "Only", "P0", vec![]));
    let batches = parallel_batches(&single);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0][0].id, "only");
}

#[test]
fn test_in_degree_tracker() {
    let mut plan = TaskPlan::new("test");
    plan.add_node(PlanNode::new("A", "A", "P0", vec![]));
    plan.add_node(PlanNode::new("B", "B", "P1", vec!["A".to_string()]));
    plan.add_node(PlanNode::new("C", "C", "P1", vec!["A".to_string()]));

    let tracker = InDegreeTracker::from_plan(&plan);
    assert_eq!(tracker.get("A"), 0);
    assert_eq!(tracker.get("B"), 1);
    assert_eq!(tracker.get("C"), 1);

    let mut tracker = InDegreeTracker::from_plan(&plan);
    let ready = tracker.mark_completed("A", &plan);
    assert_eq!(ready.len(), 2);
    assert!(ready.contains(&"B".to_string()));
    assert!(ready.contains(&"C".to_string()));
}

#[test]
fn test_in_degree_tracker_zero_degree() {
    let mut plan = TaskPlan::new("test");
    plan.add_node(PlanNode::new("A", "A", "P0", vec![]));
    let tracker = InDegreeTracker::from_plan(&plan);
    assert_eq!(tracker.zero_degree_nodes(), vec!["A"]);
}

#[test]
fn test_plan_node_ready_check() {
    let node = PlanNode::new("A", "A", "P0", vec!["dep1".to_string(), "dep2".to_string()]);
    assert!(!node.is_ready(&[]));
    assert!(!node.is_ready(&["dep1".to_string()]));
    assert!(node.is_ready(&["dep1".to_string(), "dep2".to_string()]));
}

#[test]
fn test_plan_node_defaults() {
    let node = PlanNode::new("id-1", "My Task", "Phase 1", vec![]);
    assert_eq!(node.id, "id-1");
    assert_eq!(node.label, "My Task");
    assert_eq!(node.phase, "Phase 1");
    assert!(node.spec.is_none());
    assert_eq!(node.max_retries, 2);
    assert_eq!(node.retry_count, 0);
}

#[test]
fn test_plan_node_with_spec() {
    let node = PlanNode::new("t1", "Task 1", "P0", vec![]).with_spec("Do the thing");
    assert_eq!(node.spec, Some("Do the thing".to_string()));
}

#[test]
fn test_plan_node_with_max_retries() {
    let node = PlanNode::new("t1", "Task 1", "P0", vec![]).with_max_retries(5);
    assert_eq!(node.max_retries, 5);
}

#[test]
fn test_plan_node_build_message() {
    let node = PlanNode::new("A", "Impl auth", "P1", vec![])
        .with_spec("Create login endpoint with JWT");
    let msg = node.build_message();
    assert!(msg.contains("Impl auth"));
    assert!(msg.contains("JWT"));
}

#[test]
fn test_noop_gate_always_passes() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = AgentResult {
        agent_id: "x".into(), status: AgentStatus::Failed,
        events: vec![], final_message: None, error: Some("err".into()),
    };
    let node = PlanNode::new("x", "X", "P0", vec![]);
    let config = PlanConfig::default();
    let verdict = rt.block_on(NoopGate.check(&result, &node, &config));
    assert_eq!(verdict, GateVerdict::Pass);
}

#[test]
fn test_quality_gate_trait_object() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let gates: Vec<Box<dyn QualityGate>> = vec![
        Box::new(NoopGate),
    ];
    let result = AgentResult {
        agent_id: "a".into(), status: AgentStatus::Completed,
        events: vec![], final_message: None, error: None,
    };
    let node = PlanNode::new("t", "T", "P0", vec![]);
    let config = PlanConfig::default();
    assert!(rt.block_on(gates[0].check(&result, &node, &config)).is_pass());
}

#[test]
fn test_execution_report_creation() {
    let report = ExecutionReport {
        plan_name: "test-plan".into(),
        total_nodes: 5,
        completed_nodes: 3,
        failed_nodes: 1,
        cancelled_nodes: 0,
        duration_ms: 1500,
        success: false,
        node_summaries: vec![
            NodeSummary {
                id: "task-1".into(), label: "Task 1".into(), phase: "P0".into(),
                status: code_agent_core::agent::plan::NodeStatus::Completed,
                error: None, final_message_summary: None, retry_count: 0, gate_verdict: None,
            },
        ],
    };
    assert_eq!(report.total_nodes, 5);
    assert!(!report.success);
}

#[test]
fn test_plan_store_create_and_query() -> Result<(), Box<dyn std::error::Error>> {
    let store = PlanStore::open_in_memory()?;
    let report = ExecutionReport {
        plan_name: "test-plan".into(), total_nodes: 3, completed_nodes: 2,
        failed_nodes: 1, cancelled_nodes: 0, duration_ms: 500,
        success: false, node_summaries: vec![],
    };
    let record_id = store.store_execution(&report, Some("session-001"))?;
    assert!(record_id > 0);
    let loaded = store.get_execution(record_id)?;
    assert!(loaded.is_some());
    let rec = loaded.unwrap();
    assert_eq!(rec.plan_name, "test-plan");
    assert_eq!(rec.session_id, Some("session-001".to_string()));
    assert!(!rec.success);
    Ok(())
}

#[test]
fn test_plan_store_list_executions() -> Result<(), Box<dyn std::error::Error>> {
    let store = PlanStore::open_in_memory()?;
    let ok = ExecutionReport {
        plan_name: "pass".into(), total_nodes: 1, completed_nodes: 1,
        failed_nodes: 0, cancelled_nodes: 0, duration_ms: 100,
        success: true, node_summaries: vec![],
    };
    let fail = ExecutionReport {
        plan_name: "fail".into(), total_nodes: 1, completed_nodes: 0,
        failed_nodes: 1, cancelled_nodes: 0, duration_ms: 50,
        success: false, node_summaries: vec![],
    };
    store.store_execution(&ok, None)?;
    store.store_execution(&fail, None)?;
    let all = store.list_executions(10)?;
    assert_eq!(all.len(), 2);
    assert_eq!(all.iter().filter(|r| r.success).count(), 1);
    Ok(())
}

#[test]
fn test_plan_execution_record_to_error_traces() {
    let record = PlanExecutionRecord {
        id: 1, plan_name: "test".into(), session_id: None,
        success: false, total_nodes: 2, completed_nodes: 0,
        failed_nodes: 1, cancelled_nodes: 0, duration_ms: 100,
        plan_config_json: None,
        created_at: chrono::Utc::now(),
        nodes: vec![
            PlanNodeRecord {
                id: 1, node_id: "task-1".into(), label: "Task 1".into(),
                phase: "P0".into(), status: "Failed".into(),
                error: Some("timeout".into()), retry_count: 2,
                gate_verdict: Some("Fail".into()), final_message_summary: None,
            },
            PlanNodeRecord {
                id: 2, node_id: "task-2".into(), label: "Task 2".into(),
                phase: "P0".into(), status: "Completed".into(), error: None,
                retry_count: 0, gate_verdict: None, final_message_summary: None,
            },
        ],
    };
    let traces = record.to_error_traces();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].error_type, "plan_node_failed");
    assert!(traces[0].message.contains("Task 1"));
    assert!(traces[0].message.contains("(gate: Fail)"));
}

#[test]
fn test_knowledge_gate_config_defaults() {
    let config = KnowledgeGateConfig::default();
    assert!(config.fail_on_critical);
    assert!(config.retry_on_error);
}

#[test]
fn test_task_plan_defaults() {
    let plan = TaskPlan::new("my-plan");
    assert_eq!(plan.name, "my-plan");
    assert!(plan.nodes.is_empty());
    assert!(plan.phases.is_empty());
    assert!(plan.created_at > 0);
}

#[test]
fn test_task_plan_add_phase_dedup() {
    let mut plan = TaskPlan::new("p");
    plan.add_phase("Phase 0");
    plan.add_phase("Phase 0");
    assert_eq!(plan.phases.len(), 1);
}

#[test]
fn test_plan_config_defaults() {
    let config = PlanConfig::default();
    assert_eq!(config.max_parallel, 6);
    assert_eq!(config.coder_max_iterations, 15);
    assert_eq!(config.default_max_retries, 2);
}

// ===========================================================================
// 2. Flywheel 数据飞轮测试
// ===========================================================================

fn sample_trace(error_type: &str, tool_name: &str, message: &str) -> ErrorTrace {
    ErrorTrace {
        error_type: error_type.into(),
        tool_name: tool_name.into(),
        language: "rust".into(),
        file_pattern: "src/**/*.rs".into(),
        turn_count: 3,
        message: message.into(),
        timestamp: chrono::Utc::now(),
    }
}

#[test]
fn test_flywheel_collector_record_and_analyze() {
    let mut collector = FlywheelCollector::new();
    collector.record(sample_trace("timeout", "bash", "timed out"));
    collector.record(sample_trace("timeout", "bash", "timed out again"));
    assert_eq!(collector.pending_traces(), 2);

    let report = collector.analyze();
    assert_eq!(report.traces_processed, 2);
    assert!(report.clusters_found > 0);
}

#[test]
fn test_flywheel_collector_empty_analyze() {
    let mut collector = FlywheelCollector::new();
    let report = collector.analyze();
    assert_eq!(report.traces_processed, 0);
    assert_eq!(report.suggestions_count, 0);
}

#[test]
fn test_flywheel_collector_auto_update() {
    let collector = FlywheelCollector::with_auto_update();
    // with_auto_update creates a collector that auto-updates tool descriptions
    assert_eq!(collector.pending_traces(), 0);
}

#[test]
fn test_flywheel_collector_incremental() {
    let mut collector = FlywheelCollector::new();
    collector.record(sample_trace("timeout", "bash", "timed out"));
    let suggestions = collector.analyze_incremental();
    assert!(!suggestions.is_empty());
}

#[test]
fn test_error_trace_construction() {
    let trace = sample_trace("build_failure", "bash", "cargo build failed");
    assert_eq!(trace.error_type, "build_failure");
    assert_eq!(trace.tool_name, "bash");
    assert_eq!(trace.language, "rust");
}



// ===========================================================================
// 3. Hook 系统集成测试
// ===========================================================================

struct RecorderHook { events: std::sync::Mutex<Vec<String>> }
impl RecorderHook {
    fn new() -> Self { Self { events: std::sync::Mutex::new(Vec::new()) } }
}
impl ToolHook for RecorderHook {
    fn on_event(&self, event: &ToolEvent) {
        let s = match event {
            ToolEvent::PreExecute { tool_name, .. } => format!("pre:{tool_name}"),
            ToolEvent::PostExecute { tool_name, .. } => format!("post:{tool_name}"),
            ToolEvent::PreModelCall { .. } => "pre:model".into(),
            ToolEvent::PostModelCall { .. } => "post:model".into(),
            ToolEvent::SessionCreate { thread_id, .. } => format!("create:{thread_id}"),
            ToolEvent::SessionDestroy { thread_id, .. } => format!("destroy:{thread_id}"),
            ToolEvent::PreCommand { command, .. } => format!("pre:cmd:{command}"),
            ToolEvent::PostCommand { command, .. } => format!("post:cmd:{command}"),
        };
        self.events.lock().unwrap().push(s);
    }
}

struct CounterHook(AtomicUsize);
impl CounterHook { fn new() -> Self { Self(AtomicUsize::new(0)) } fn count(&self) -> usize { self.0.load(Ordering::SeqCst) } }
impl ToolHook for CounterHook {
    fn on_event(&self, _: &ToolEvent) { self.0.fetch_add(1, Ordering::SeqCst); }
}

fn make_tool_result() -> code_agent_protocol::ToolResultMessage {
    code_agent_protocol::ToolResultMessage {
        tool_call_id: "c1".into(), output: Some("ok".into()), error: None,
    }
}

#[test]
fn test_hook_registry_basic() {
    let h = Arc::new(CounterHook::new());
    let mut reg = HookRegistry::new();
    reg.register(h.clone());
    assert_eq!(reg.len(), 1);
    assert!(!reg.is_empty());

    reg.dispatch(&ToolEvent::PreExecute {
        call_id: "c1".into(), tool_name: "read".into(), params_hash: "h".into(),
    });
    assert_eq!(h.count(), 1);
}

#[test]
fn test_hook_registry_dispatch_all_event_types() {
    let rec = Arc::new(RecorderHook::new());
    let mut reg = HookRegistry::new();
    reg.register(rec.clone());

    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "bash".into(), params_hash: "h".into() });
    reg.dispatch(&ToolEvent::PostExecute { call_id: "c1".into(), tool_name: "bash".into(), result: make_tool_result(), duration_ms: 42 });
    reg.dispatch(&ToolEvent::PreModelCall { snapshot: ModelCallSnapshot { message_count: 5, estimated_input_tokens: 200, model: None } });
    reg.dispatch(&ToolEvent::PostModelCall { snapshot: ModelCallSnapshot { message_count: 5, estimated_input_tokens: 200, model: None }, response_tokens: 100, elapsed_ms: 300 });
    reg.dispatch(&ToolEvent::SessionCreate { thread_id: "t1".into(), session_id: "s1".into(), system_instructions: "be helpful".into() });
    reg.dispatch(&ToolEvent::SessionDestroy { thread_id: "t1".into(), session_id: "s1".into() });
    reg.dispatch(&ToolEvent::PreCommand { command: "help".into(), args: "".into() });
    reg.dispatch(&ToolEvent::PostCommand { command: "help".into(), args: "".into(), success: true, elapsed_ms: 5 });

    let events = rec.events.lock().unwrap();
    assert_eq!(events.len(), 8);
    assert_eq!(events[0], "pre:bash");
    assert_eq!(events[4], "create:t1");
    assert_eq!(events[7], "post:cmd:help");
}

#[test]
fn test_hook_registry_multiple_hooks() {
    let c1 = Arc::new(CounterHook::new());
    let c2 = Arc::new(CounterHook::new());
    let mut reg = HookRegistry::new();
    reg.register(c1.clone());
    reg.register(c2.clone());

    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "t".into(), params_hash: "h".into() });
    assert_eq!(c1.count(), 1);
    assert_eq!(c2.count(), 1);
}

#[test]
fn test_hook_registry_remove_middle() {
    let c1 = Arc::new(CounterHook::new());
    let c2 = Arc::new(CounterHook::new());
    let c3 = Arc::new(CounterHook::new());
    let mut reg = HookRegistry::new();
    reg.register(c1.clone());
    reg.register(c2.clone());
    reg.register(c3.clone());

    reg.remove(1).ok();
    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "t".into(), params_hash: "h".into() });

    assert_eq!(c1.count(), 1);
    assert_eq!(c2.count(), 0, "Removed hook should not fire");
    assert_eq!(c3.count(), 1);
}

#[test]
fn test_hook_registry_out_of_range_remove() {
    let mut reg = HookRegistry::new();
    assert!(reg.remove(0).is_err());
    reg.register(Arc::new(NoopHook));
    assert!(reg.remove(1).is_err());
}

#[test]
fn test_hook_dispatch_empty_registry() {
    let reg = HookRegistry::new();
    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "t".into(), params_hash: "h".into() });
}

#[test]
fn test_hook_default_and_new() {
    let reg = HookRegistry::default();
    assert!(reg.is_empty());
    let reg2 = HookRegistry::new();
    assert!(reg2.is_empty());
}

#[test]
fn test_noop_hook_all_events() {
    let h = Arc::new(NoopHook);
    let mut reg = HookRegistry::new();
    reg.register(h);

    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "r".into(), params_hash: "h".into() });
    reg.dispatch(&ToolEvent::PostExecute { call_id: "c1".into(), tool_name: "r".into(), result: make_tool_result(), duration_ms: 1 });
    reg.dispatch(&ToolEvent::PreModelCall { snapshot: ModelCallSnapshot { message_count: 0, estimated_input_tokens: 0, model: None } });
    reg.dispatch(&ToolEvent::PostModelCall { snapshot: ModelCallSnapshot { message_count: 0, estimated_input_tokens: 0, model: None }, response_tokens: 0, elapsed_ms: 0 });
    reg.dispatch(&ToolEvent::SessionCreate { thread_id: "".into(), session_id: "".into(), system_instructions: "".into() });
    reg.dispatch(&ToolEvent::SessionDestroy { thread_id: "".into(), session_id: "".into() });
    reg.dispatch(&ToolEvent::PreCommand { command: "h".into(), args: "".into() });
    reg.dispatch(&ToolEvent::PostCommand { command: "h".into(), args: "".into(), success: true, elapsed_ms: 0 });
}

#[test]
fn test_hook_fifty_hooks() {
    let hooks: Vec<Arc<CounterHook>> = (0..50).map(|_| Arc::new(CounterHook::new())).collect();
    let mut reg = HookRegistry::new();
    for h in &hooks { reg.register(h.clone()); }

    reg.dispatch(&ToolEvent::PreExecute { call_id: "c1".into(), tool_name: "t".into(), params_hash: "h".into() });

    for (i, h) in hooks.iter().enumerate() {
        assert_eq!(h.count(), 1, "Hook {i}");
    }
}
