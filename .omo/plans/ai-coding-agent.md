# ai-coding-agent - Work Plan

## TL;DR (For humans)

**What you'll get:** An AI coding agent that lives in your terminal, IDE, and browser — it understands your codebase, helps you write, fix, and refactor code, runs tests, and improves over time. Think Claude Code meets Cursor but open-architecture, with a plugin ecosystem via MCP.

**Why this approach:** We build the agent core in Rust for speed and security (no Java/Node runtime needed), use a DIY agent loop instead of a heavy framework (the AI logic is only ~2% of the code), and adopt MCP as the universal tool plug-in standard so the community can extend it. Code indexing uses AST graphs + vector search — the same approach proven by Aider to use 95% less context than naive methods.

**What it will NOT do:** Train its own AI models, run as a cloud-only service, replace your version control, or require an internet connection for core editing features.

**Effort:** XL — 8 execution waves, ~6-9 months for MVP
**Risk:** Medium — primary risks are context management at scale and cross-language code understanding quality
**Decisions to sanity-check:** (1) Rust vs Python for agent core, (2) self-built ReAct Loop vs LangChain, (3) graph-first indexing vs pure embeddings, (4) MCP as universal tool protocol, (5) local-first vs cloud-first architecture

Your next move: **Approve** to begin execution, or request a high-accuracy review first.

---

> TL;DR (machine): XL effort, Medium risk, 8-wave execution. Rust monorepo with Python eval layer. Deliverables: ReAct agent engine, MCP tool integration, graph+vector code index, eval framework (SWE-bench-Live compatible), CLI/IDE/Web surfaces. ~6-9 month MVP.

## Scope
### Must have
- Rust core engine with ReAct agent loop (session management, tool routing, context compaction)
- Code understanding: tree-sitter AST parsing, call graph construction, BM25+vector hybrid retrieval
- Tool integration: sandboxed shell, filesystem operations, LSP intelligence, git operations, MCP client+server
- Evaluation framework: custom runner with SWE-bench-Live and HumanEval adapters, metrics computation
- Product: interactive TUI, headless CLI exec mode, JSON-RPC App Server for IDE integration
- VS Code extension using App Server protocol
- Lightweight Web API for remote/browser use
- OpenTelemetry-based observability, step-level logging, data flywheel
- Security: two-dimensional permission model (capability × approval mode), sandboxed execution

### Must NOT have (guardrails, anti-slop, scope boundaries)
- NO custom LLM training or fine-tuning infrastructure
- NO cloud-only deployment (local-first always)
- NO enterprise SSO, user management, or team collaboration features in MVP
- NO proprietary embedding model training
- NO mobile or tablet clients
- NO compiler/interpreter implementation
- NO replacement for version control workflows
- NO data collection without explicit user consent

## Verification strategy
> Zero human intervention targeting — all verification is agent-executed.
- Test decision: **TDD for core engine + tests-after for integrations** | Framework: Rust `cargo test` + Python `pytest`
- Evidence: .omo/evidence/task-<N>-ai-coding-agent.<ext>
- Per-task verification: cargo test, cargo clippy, cargo fmt --check
- Integration tests require sandboxed Docker environment
- Eval framework verified against known SWE-bench-Live baselines

## Execution strategy
### Parallel execution waves
The plan is organized into 8 waves (grouped by dependency). Within each wave, independent todos fire in parallel. Sequential dependencies only where a wave produces artifacts the next wave consumes.

**Wave dependency chain:**
```
Wave 1 (Foundation) → Wave 2 (Agent Engine) → Wave 3 (Code Understanding) → Wave 4 (Tool Chain)
                                                      ↓                          ↓
                                             Wave 5 (Eval Framework) → Wave 6 (CLI/TUI) → Wave 7 (IDE/Web)
                                                                                          ↓
                                                                                Wave 8 (Integration + Data Flywheel)
```

### Dependency matrix
| Wave | Depends on | Blocks | Parallelizable |
|------|-----------|--------|----------------|
| 1 | None | 2, 3, 4 | All Wave 1 tasks |
| 2 | 1 | 5, 6, 7 | 3, 4 |
| 3 | 1 | 5 | 2, 4 |
| 4 | 1 | 6, 7 | 2, 3 |
| 5 | 2, 3 | 8 | 6 |
| 6 | 2, 4 | 8 | 5, 7 |
| 7 | 2, 4 | 8 | 5, 6 |
| 8 | 5, 6, 7 | None | All Wave 8 tasks |

## Todos
> Implementation + Test = ONE todo. Never separate.

### Wave 1 (Foundation)

- [x] 1. Initialize Rust Cargo workspace with 7-crate structure
  What: Create `code-agent-rs/` Cargo workspace. Crates: `core` (agent engine), `tools` (tool implementations), `codex` (code understanding), `eval` (evaluation runner), `cli` (CLI entry), `app-server` (IDE protocol), `protocol` (shared types). Root Cargo.toml with workspace members, dependency sharing, profile overrides (dev + release). .cargo/config.toml with optimization flags.
  Wave 1 | Blocked by: None | Blocks: Wave 2, 3, 4
  References: Codex CLI workspace at openai/codex/codex-rs/Cargo.toml; Rust cargo workspaces docs
  Acceptance: `cargo build --workspace` succeeds from workspace root; `cargo test --workspace` runs harness; each crate has valid Cargo.toml
  QA: Happy: full workspace build <60s release mode; Failure: malformed Cargo.toml in one crate → workspace still builds valid crates; Edge: no crates yet → empty workspace warning
  Commit: Y | chore(w1): initialize Rust workspace with 7 crates

- [x] 2. Define shared protocol types and core data models
  What: In `protocol/`: SessionId, ThreadId, TurnId, Message (enum: UserMessage, AssistantMessage, ToolCall, ToolResult), ToolCall (id, name, args JSON), ToolResult (tool_call_id, output/error), TurnInput, ResponseEvent, SessionStatus, PermissionMode (Auto/Permit/Block), CapabilityLevel (Read/Edit/Exec). All Serde Serialize/Deserialize + Clone + Debug + doc comments. Tests for round-trip serialization.
  Wave 1 | Blocked by: None | Blocks: Wave 2
  References: Codex protocol types at openai/codex/codex-rs/core/src/lib.rs; MCP specification base types
  Acceptance: `cargo test` passes; all types round-trip JSON serialize/deserialize; doc tests compile; no unwrap() in production code
  QA: Happy: serialize Message::ToolCall → JSON → deserialize → identical; Failure: malformed JSON → deserialize returns error; Edge: empty ToolResult handled
  Commit: Y | feat(w1): define core protocol types and data models

- [x] 3. Set up CI/CD pipeline with GitHub Actions
  What: GitHub Actions: (1) CI — cargo check, cargo clippy -- -D warnings, cargo fmt --check, cargo test --workspace on push/PR; (2) Release — build linux-x64, macos-x64/arm64, windows-x64 binaries; (3) Audit — weekly cargo audit. rust-toolchain.toml pinning stable. clippy.toml with strict rules (pedantic, nursery). Dependabot for crate updates.
  Wave 1 | Blocked by: 1 | Blocks: All subsequent waves
  References: openai/codex .github/workflows/; Rust CI best practices
  Acceptance: push triggers CI → all jobs green; clippy passes zero warnings; release workflow produces 4 platform binaries; audit reports clean
  QA: Happy: PR pushed → CI checks, all pass; Failure: clippy violation → CI fails, output shows location; Edge: no Rust installed → CI runner handles
  Commit: N (infra)

- [x] 4. Create Python evaluation environment with PyO3 bridge
  What: `eval-py/` with pyproject.toml (uv/pdm), pytest, docker SDK, click CLI. EvalTask and EvalResult Pydantic models. `eval-bridge/` Rust crate with PyO3 bindings exposing eval runner results as Rust structs. Test: round-trip EvalResult between Python and Rust.
  Wave 1 | Blocked by: None | Blocks: Wave 5
  References: SWE-bench-Live runner at microsoft/SWE-bench-Live/evaluation; PyO3 docs
  Acceptance: pytest tests/ passes; PyO3 bridge compiles; Rust calls Python eval function → correct result; Docker-based sandbox hello-world runs
  QA: Happy: EvalResult from Python → Rust deserializes correctly; Failure: PyO3 import error → compile-time failure; Edge: Python not installed → clear error
  Commit: Y | feat(w1): scaffold Python eval environment with PyO3 bridge

### Wave 2 (Agent Engine)

> **Interface spec: Agent Engine ↔ Code Understanding** (applies to Wave 2 & 3 parallel execution)
> The `CodeContextBuilder` (built in Todo 14) is consumed by the Agent Engine as a **built-in tool** named `search_codebase`.
> - **Tool schema**: `{ name: "search_codebase", description: "Search codebase for relevant code", input: { query: string, scope?: string[], max_results?: number }, output: ScoredResult[] }`
> - **Trait**: `CodexEngine` trait in `protocol` crate — `fn search(&self, query: &str, scope: &[FileFilter]) -> Vec<ScoredResult>`
> - **Implementation**: `codex` crate implements the trait; `core` crate uses it via dependency injection.
> - **Fallback**: When index unavailable, falls back to `grep` + `glob` tool calls (agent-driven exploration).
> This interface MUST be documented in `protocol/src/codex.rs` before Wave 2 and Wave 3 execute in parallel.

- [x] 5. Implement ReAct agent loop with session management
  What: In `core/src/agent/`: Session struct with thread manager, tool registry, model client, context manager. `run_turn(TurnInput) -> Vec<ResponseEvent>` — the ReAct loop: build prompt → call model → parse response → execute tools → append results → check termination → loop. Support Interrupt operation. ThreadManager for concurrent sessions. Async with tokio.
  Wave 2 | Blocked by: 1, 2 | Blocks: Wave 5, 6, 7
  References: Codex session at openai/codex/codex-rs/core/src/session/session.rs:27-48; Claude Code queryLoop at .omo/research/agent-architectures.md:52-70
  Acceptance: session completes 3-turn ReAct loop with mock model; interruption mid-turn stops execution; ThreadManager tracks 3 concurrent sessions
  QA: Happy: user prompt → tool call → observe → answer produced; Failure: invalid tool call → retry graceful; Stress: 100-turn session → no memory leak
  Commit: Y | feat(w2): implement ReAct agent loop with session management

- [x] 6. Build multi-provider model client abstraction
  What: In `core/src/model/`: ModelClient trait (send prompt, stream response, tool support). Implementations: OpenAI (Responses API, SSE streaming), Anthropic (Messages API), OpenAICompat (generic). Config via TOML. Features: tool injection, token tracking, retry with exponential backoff, rate limiting, streaming events via channel.
  Wave 2 | Blocked by: 1, 2 | Blocks: 5
  References: Codex model client at openai/codex/codex-rs/core/src/client.rs:11-14; OpenAI Responses API; Anthropic Messages API
  Acceptance: all 3 providers work with mock server; token tracking accurate; retry 3x before failure; config switching works
  QA: Happy: OpenAI client streams response + tool calls; Failure: 429 → backoff retry succeeds; Edge: timeout → graceful error
  Commit: Y | feat(w2): implement multi-provider model client

- [x] 7. Implement 5-layer context compaction pipeline
  What: In `core/src/context/`: ContextManager — 5 compaction layers: BudgetReduction (truncate oversized outputs), Snip (trim deep history), Microcompact (cache pressure), ContextCollapse (aggregate long histories), AutoCompact (LLM summarization). Thresholds: 70% monitor, 85% compress, 95% evict. Preserve system prompt + last 3 turns.
  Wave 2 | Blocked by: 5 | Blocks: None
  References: Claude Code compaction at .omo/research/agent-architectures.md:310-319; Codex compact at openai/codex/codex-rs/core/src/codex/compact.rs
  Acceptance: compaction triggers at 85%; token drops >40%; system prompt + last 3 turns preserved; 100-turn stress <64K tokens
  QA: Happy: 50-turn conversation 80K→35K tokens; Failure: compaction disabled → context overflow handled; Bench: compaction <2s
  Commit: Y | feat(w2): implement 5-layer context compaction pipeline

- [x] 8. Build tool registry and routing system
  What: In `core/src/tools/`: ToolRegistry (register/list/get), Tool trait (name, description, input_schema, execute). ToolRouter dispatching by name. Built-in tools: read_file, write_file, edit_file (search-replace), list_dir, grep, glob, bash (sandboxed), web_search. Permission enforcement: capability × approval before execution. Streaming tool results.
  Wave 2 | Blocked by: 1, 2 | Blocks: Wave 4
  References: Codex ToolRouter at openai/codex/codex-rs/core/src/session/tests.rs:71; MCP tool schema spec
  Acceptance: registry registers/retrieves 10 tools; router dispatches to correct handler; permission denied → graceful error
  QA: Happy: read_file returns contents; Failure: invalid tool name → clear error; Edge: concurrent tool calls → no race
  Commit: Y | feat(w2): implement tool registry and routing

- [x] 9. Implement prompt injection defense and content safety layer
  What: In `core/src/safety/`: ContentSafetyLayer — sanitize tool results before they enter agent context. Pipeline: (1) Classify tool result content as allowed/suspicious/blocked, (2) Strip or quarantine system-instruction-like content from file reads and web content, (3) Log all suspicious content for audit. Defense patterns: instruction boundary markers, role-confusion detection, output encoding verification. `SafetyConfig` — user-configurable strictness (off/warn/block). Reference known patterns: Anthropic PreToolUse hooks, OpenAI content filter.
  Wave 2 | Blocked by: 8 | Blocks: None
  References: Anthropic PreToolUse hooks at docs.anthropic.com; prompt injection taxonomies at OWASP LLM Top 10; .omo/research/agent-architectures.md safety section
  Acceptance: injected instruction in tool output is detected and blocked; normal code content passes through; blocked content logged with tool call context; safety layer runs before context enters model prompt
  QA: Happy: normal file read content passes through unmodified; Failure: "Ignore previous instructions" in web content → blocked with warning; Edge: safety disabled → content passes through with audit log
  Commit: Y | feat(w2): implement prompt injection defense layer

- [x] 10. Implement multi-agent spawning and coordination
  What: In `core/src/agent/`: SubAgentManager — spawn with isolated context + inherited config. Tools: spawn_agent, send_message, wait_agent, list_agents, close_agent. Max depth 2, max 6 parallel. Sub-agents share tool registry, independent session state. Results streamed back as events.
  Wave 2 | Blocked by: 5, 8 | Blocks: None
  References: Codex multi-agent v2 at openai/codex/codex-rs/core/src/agent/control.rs:84-102
  Acceptance: spawn → send → receive → close; max depth enforced; 6 concurrent without deadlock; parent interruption cascades
  QA: Happy: 2 sub-agents parallel file search; Failure: sub-agent crash → parent gets error; Edge: depth violation → rejected
  Commit: Y | feat(w2): implement multi-agent spawning

### Wave 3 (Code Understanding)

- [x] 11. Build AST-based code indexer using tree-sitter
  What: In `codex/src/indexer/`: CodeIndexer — walk workspace, parse with tree-sitter grammars (Python, TS, Rust, Go, Java), extract semantic units (function/class/import/declaration), build symbol table (file_path, line_range, symbol_name, kind). Incremental via notify watcher. SQLite storage. Respect .gitignore and .agentignore.
  Wave 3 | Blocked by: 1 | Blocks: 12, 13, 14
  References: Cursor indexing at .omo/research/code-understanding.md:42-47; tree-sitter docs; Aider repo-map
  Acceptance: 100-file project indexed <30s; correct symbol counts per language; incremental detects single file change; respects .gitignore
  QA: Happy: new file → index updated <1s; Failure: syntax error file → skipped with warning; Bench: 10K file repo <5min initial
  Commit: Y | feat(w3): implement tree-sitter AST code indexer

- [x] 12. Build call graph and dependency graph
  What: In `codex/src/graph/`: CallGraph — directional edges (caller→callee). DependencyGraph — module-level imports. Build from AST traversal + symbol resolution. Queries: get_callers, get_callees, get_dependents, get_dependencies, get_impact_analysis(symbol, depth). SQLite persistence. Incremental patch on file change.
  Wave 3 | Blocked by: 11 | Blocks: 13
  References: Gortex graph engine at .omo/research/code-understanding.md:83-91; Code2Flow
  Acceptance: 50-function graph → correct edges; dependency graph tracks imports; impact analysis depth-tiered; incremental patch works
  QA: Happy: rename → impact shows all callers; Failure: dynamic dispatch → logged as uncertain; Bench: 1000 node graph query <10ms
  Commit: Y | feat(w3): build call graph and dependency analysis

- [x] 13. Implement hybrid retrieval pipeline (BM25 + Vector + Graph)
  What: In `codex/src/retrieval/`: 3-stage retriever. BM25 lexical (symbol names + docs + source). Vector (code embeddings via API: voyage-code-2 / text-embedding-3-large; local: all-MiniLM-L6-v2 fallback). Graph reranking (centrality boost). RRF fusion. Retriever.search(query, top_k, filters) -> Vec<ScoredResult>. Metadata filters: file_path, symbol_kind, language.
  Wave 3 | Blocked by: 11, 12 | Blocks: 14
  References: Cursor retrieval at .omo/research/code-understanding.md; Gortex hybrid; RRF algorithm
  Acceptance: hybrid returns relevant results; BM25-alone vs hybrid shows improvement; offline model works; filters narrow results correctly
  QA: Happy: "find auth handling" → auth symbols ranked high; Failure: empty corpus → empty results; Bench: query <500ms p95/10K symbols
  Commit: Y | feat(w3): implement hybrid BM25+vector+graph retrieval

- [x] 14. Build agent code context retrieval
  What: In `codex/src/context/`: CodeContextBuilder — embed task → hybrid search → 1-hop graph traversal → rank (relevance + recency + centrality) → format context block. Target 8K tokens with priority truncation. LRU cache for repeated queries. Context formatting: file_path:line_range + symbol signature.
  Wave 3 | Blocked by: 13 | Blocks: Wave 5
  References: Cursor context assembly; Priompt priority compilation; .omo/research/code-understanding.md context retrieval section
  Acceptance: retrieves <8K tokens for task; relevant files rank higher; cache hit <10ms; formatting produces valid prompt section
  QA: Happy: "fix login bug" → login controller + auth middleware + session util; Failure: no relevant code → empty context; Bench: <2s p95
  Commit: Y | feat(w3): build agent code context retrieval

- [x] 15. Implement incremental file watcher and index sync
  What: In `codex/src/watcher/`: Cross-platform watcher (notify crate, debounce 150ms). On change: parse → symbol diff → patch index + graph + embeddings. On create: full parse + index. On delete: remove from all indexes. Content hash (BLAKE3) avoids redundant re-index on non-content saves.
  Wave 3 | Blocked by: 11, 12 | Blocks: None
  References: Cursor Merkle tree sync; Gortex incremental patch (~200ms); .omo/research/code-understanding.md sync problem section
  Acceptance: file edit patched <500ms; content-identical save no re-index; batch 10 changes handled; watcher pauses during full re-index
  QA: Happy: edit function → graph + embeddings updated; Failure: delete during index → no crash; Bench: 100 concurrent saves <5s
  Commit: Y | feat(w3): implement incremental file watcher

### Wave 4 (Tool Chain)

- [x] 16. Implement sandboxed shell execution (Linux MVP, Docker fallback for macOS/Windows)
  What: In `tools/src/shell/`: **MVP scope: Linux via bubblewrap + Landlock + seccomp.** macOS: fallback to Docker container execution (sandbox-exec Seatbelt is deprecated). Windows: fallback to Docker container execution (restricted tokens + job objects in post-MVP). Config: allow_network, allow_write, allowed_paths. Features: timeout (60s), output streaming, working directory, env vars. UnifiedExecProcessManager. **Security review gate** before merging. Post-MVP: native macOS Seatbelt and Windows sandbox (tracked as separate tech-debt items).
  Wave 4 | Blocked by: 1 | Blocks: Wave 6
  References: Codex sandbox at openai/codex/codex-rs/core/src/lib.rs:48; bubblewrap docs; Docker API sandbox pattern
  Acceptance: Docker container runs on all 3 platforms; bubblewrap works on Linux; timeout kills long command; no-network blocks curl; security review recorded in .omo/evidence/
  QA: Happy: ls -la returns listing; Failure: rm -rf / blocked; Security: security review gate evidence; Edge: 10MB stdout → truncated gracefully
  Commit: Y | feat(w4): implement Linux/MVP sandboxed shell with Docker fallback

- [x] 17. Build LSP integration layer
  What: In `tools/src/lsp/`: LspClient managing per-language LSP servers (pyright, rust-analyzer, typescript-language-server, jdtls, gopls). Tools: get_definition, get_references, get_hover, get_completions, get_diagnostics, get_symbols. Connection pooling. Cold-start: pre-start common servers. Restart on crash.
  Wave 4 | Blocked by: 1 | Blocks: Wave 6
  References: agent-lsp project; LSP specification; .omo/research/toolchain-integration.md LSP section
  Acceptance: LSP client starts pyright → returns diagnostics; go-to-definition cross-file; server restart on crash; concurrent requests handled
  QA: Happy: get_definition → file+line; Failure: LSP not installed → install instructions; Bench: cold <3s, warm <100ms
  Commit: Y | feat(w4): implement LSP integration layer

- [x] 18. Build Git integration tools
  What: In `tools/src/git/`: GitClient via git2 crate + CLI fallback. Tools: git_status, git_diff, git_commit, git_add, git_branch, git_log, git_show. Safety: no force-push, no reset-hard without permission. PR via gh CLI: pr_create, pr_list, pr_merge. Diff truncated at 1000 lines.
  Wave 4 | Blocked by: 1 | Blocks: Wave 6
  References: git2 crate; Codex exec policy rules; GitHub CLI
  Acceptance: status returns correct states; commit with message created; log returns history; gh integration detected
  QA: Happy: stage + commit with message; Failure: not a git repo → clear error; Safety: force-push → blocked
  Commit: Y | feat(w4): implement git integration tools

- [x] 19. Implement MCP client and server
  What: In `tools/src/mcp/`: McpClientManager — connect, tools/list, tools/call, receive notifications. McpServer exposing agent capabilities as MCP tools. Protocol: JSON-RPC 2.0, capability negotiation, lifecycle, progress. Tool caching. Health monitoring with reconnect. Use rmcp crate.
  Wave 4 | Blocked by: 1, 8 | Blocks: Wave 6
  References: MCP spec at modelcontextprotocol.io; rmcp Rust SDK; .omo/research/toolchain-integration.md MCP section
  Acceptance: client connects, discovers 3 tools, calls one; server registers tools; disconnect handled; caching <5ms
  QA: Happy: agent uses external MCP weather tool; Failure: server unreachable → continues without; Edge: 100 tool defs → parsed
  Commit: Y | feat(w4): implement MCP client and server

### Wave 5 (Evaluation)

- [x] 20. Build Docker-based evaluation runner
  What: In `eval/` and `eval-py/`: EvalRunner — parallel task executor via Docker. SandboxPool — pre-warm containers. Task JSON format: setup_commands, test_commands, expected_output, timeout. EvalOrchestrator — load, dispatch, collect results, compute metrics. Checkpoint/resume. Result caching by task + agent config hash.
  Wave 5 | Blocked by: 5, 11 | Blocks: Wave 8
  References: SWE-bench-Live runner at microsoft/SWE-bench-Live/evaluation; SWE-agent harness
  Acceptance: 5 test tasks in Docker executed; results parsed correctly; cache returns on re-run; 4-container pool works
  QA: Happy: eval produces JSON pass/fail per task; Failure: Docker unavailable → graceful error; Bench: 100 tasks <10min/4 containers
  Commit: Y | feat(w5): implement Docker-based evaluation runner

- [x] 21. Implement HumanEval and SWE-bench-Live adapters
  What: In `eval/`: Adapter trait for benchmarks. HumanEvalAdapter — load function signatures, run generated code, compute pass@k. SWEBenchLiveAdapter — download from huggingface, setup repo at base, run agent, apply patch, run FAIL_TO_PASS/PASS_TO_PASS. Normalized BenchmarkResult. Support lite subsets.
  Wave 5 | Blocked by: 20 | Blocks: Wave 8
  References: HumanEval paper; SWE-bench-Live at github.com/microsoft/SWE-bench-Live; .omo/research/eval-benchmarks.md
  Acceptance: HumanEval runs 10 tasks → pass@1; SWE-bench-Live runs 5 lite → valid results; timeout handled
  QA: Happy: task resolved → patch + PASS; Failure: timeout → FAIL with reason; Bench: 50 SWE-bench-Lite <30min
  Commit: Y | feat(w5): implement HumanEval and SWE-bench-Live adapters

- [x] 22. Build metrics computation and reporting
  What: In `eval/`: MetricsCalculator — pass@k (unbiased), resolve rate, edit similarity, avg tokens/task, avg turns/task, cost/task. ReportGenerator — HTML/Markdown with summary, per-task detail, regression comparison, trend charts. JSON export. Statistical significance: paired bootstrap for regression.
  Wave 5 | Blocked by: 20, 21 | Blocks: Wave 8
  References: pass@k unbiased estimator; CodeBLEU; .omo/research/eval-benchmarks.md metrics section
  Acceptance: pass@1 computed correctly; regression detects 10% drop p<0.05; HTML renders; JSON matches schema
  QA: Happy: +5% improvement → trending up; Failure: no previous run → baseline; Edge: zero pass → report still generates
  Commit: Y | feat(w5): implement metrics computation and reporting

- [x] 23. Implement continuous evaluation CI pipeline
  What: GitHub Actions eval workflow. On PR: build agent → HumanEval lite (50 tasks) → SWE-bench-Lite (50 tasks) → metrics → compare with main → post PR comment. Model A/B config comparison. Dashboard: historical metrics with Chart.js.
  Wave 5 | Blocked by: 22 | Blocks: Wave 8
  References: SWE-bench leaderboard; LiveCodeBench continuous update; .omo/research/eval-benchmarks.md continuous eval section
  Acceptance: workflow runs on PR; PR comment posted; regression → workflow fails; dashboard loads history
  QA: Happy: improvement → comment shows +3%; Failure: timeout → partial results; Bench: full eval <60min GHA limit
  Commit: Y | feat(w5): implement continuous evaluation CI pipeline

### Wave 6 (CLI/TUI)

- [x] 24. Build interactive Terminal UI (TUI)
  What: In `cli/src/tui/`: Ratatui-based terminal. Layout: chat pane (scrollable, colored), input bar (history, line edit), status bar (session info, tokens, mode). Async event handling. Features: multi-line input, command palette (/edit, /debug, /review), Ctrl+C interrupt, auto-scroll, search. Insta snapshot tests.
  Wave 6 | Blocked by: 5, 16 | Blocks: Wave 8
  References: Codex TUI at openai/codex/codex-rs/tui/; Ratatui framework; Claude Code TUI
  Acceptance: UI renders and responds; event stream renders correctly; Ctrl+C interrupts; snapshot tests match
  QA: Happy: query → reasoning + tool calls + answer displayed; Failure: agent error → red in chat pane; Edge: resize → reflow
  Commit: Y | feat(w6): implement interactive Terminal UI

- [x] 25. Build headless exec mode for CI
  What: In `cli/src/exec/`: ExecCli — non-interactive. Usage: `code-agent exec "fix lint errors"`. Output: JSON event stream to stdout or final summary. Flags: --model, --timeout, --output json|text, --cwd. Exit codes: 0 success, 1 error, 2 tool error, 3 timeout. Stdin pipe support.
  Wave 6 | Blocked by: 5, 16 | Blocks: Wave 8
  References: Codex ExecCli at openai/codex/codex-rs/cli/src/main.rs:124
  Acceptance: exec produces JSON output; exit code non-zero on error; --help shows options; stdin pipe works
  QA: Happy: echo "fix bug" | code-agent exec → fix returned; Failure: timeout → exit 3; Edge: empty input → usage hint
  Commit: Y | feat(w6): implement headless exec mode

- [x] 26. Build JSON-RPC App Server for IDE integration
  What: In `app-server/`: JSON-RPC 2.0 server over stdio + optional TCP. ThreadManager (start/resume/fork/list/archive), TurnManager (start/submit/interrupt), EventStream (subscribe to agent events). Notifications: TurnStarted, AgentMessageDelta, ToolCallBegin/End, PatchCreated, SessionStatus. SQLite persistence.
  Wave 6 | Blocked by: 5, 16 | Blocks: Wave 7
  References: Codex App Server at openai/codex/codex-rs/app-server/; Codex protocol at openai/codex/codex-rs/app-server-protocol/
  Acceptance: server starts, accepts connection; start thread → submit → receive events; fork creates independent copy; interrupt works
  QA: Happy: IDE starts thread → receives streaming diffs; Failure: disconnect → thread continues, reattachable; Bench: 10 concurrent threads
  Commit: Y | feat(w6): implement JSON-RPC App Server

- [x] 27. Create CLI entry point with config management
  What: In `cli/src/bin/`: Subcommands: tui, exec, app-server, config, eval, doctor. Config: TOML at ~/.config/code-agent/ and .code-agent/ (project). ConfigBuilder with layered resolution (builtin → global → project → flags).
  Wave 6 | Blocked by: 24, 25, 26 | Blocks: Wave 8
  References: Codex config at openai/codex/codex-rs/config/; Claude Code config
  Acceptance: CLI dispatches subcommands; config merge correct; doctor detects deps; --help shows all subcommands
  QA: Happy: code-agent tui launches TUI; Failure: missing config → created with defaults; Edge: conflicting layers → flags win
  Commit: Y | feat(w6): create CLI entry with config management

### Wave 7 (IDE/Web)

- [x] 28. Build VS Code extension with App Server client
  What: TypeScript `code-agent-vscode/`. Launch App Server binary as subprocess, JSON-RPC over stdio. Features: chat panel (webview + streaming), inline editing (diff + accept/reject), semantic search, diagnostics decorations, status bar. extension.ts + TreeView + WebView.
  Wave 7 | Blocked by: 26 | Blocks: Wave 8
  References: Codex VS Code extension at openai/codex/apps/vscode/; VS Code extension API
  Acceptance: extension activates and connects; chat renders streaming; accept applies to document; reconnect works
  QA: Happy: open file → ask fix → diff → accept → file updated; Failure: App Server not found → install prompt; Edge: external file change → warn
  Commit: Y | feat(w7): implement VS Code extension

- [x] 29. Build web API surface
  What: In `code-agent-web/`: REST + SSE endpoints: POST /threads, POST /threads/:id/turns (SSE stream), GET /threads/:id/events, DELETE /threads/:id. Optional frontend: Monaco editor + chat UI. Auth: API key or localhost default. CORS.
  Wave 7 | Blocked by: 26 | Blocks: Wave 8
  References: OpenAI Codex Web architecture; FastAPI SSE; SSE spec
  Acceptance: server starts; POST returns thread_id; SSE delivers events; concurrent threads isolated; CORS set
  QA: Happy: curl POST → SSE stream of events; Failure: invalid thread_id → 404; Bench: 50 concurrent connections
  Commit: Y | feat(w7): implement web API surface

- [x] 30. Implement diff visualization and edit acceptance
  What: Shared DiffView component (TUI + VS Code + Web). Parse unified diff, render side-by-side/inline. EditManager — track pending edits, apply/discard, group related edits. Modes: auto, per-edit, per-file. Undo: backup to .code-agent/backups/. Multi-file atomic coordination.
  Wave 7 | Blocked by: 24, 28 | Blocks: Wave 8
  References: Cursor diff overlay; Claude Code edit review; Codex patch system
  Acceptance: diff view renders 3-way; per-edit mode accepts one; undo restores backup; multi-file atomic works
  QA: Happy: 3-file edit → review → accept 2, reject 1 → only 2 applied; Failure: apply interrupted → rollback; Edge: binary → skip
  Commit: Y | feat(w7): implement diff visualization and edit acceptance

- [x] 31. Implement feedback collection and rating system
  What: In `core/src/feedback/`: FeedbackCollector — thumbs up/down, comment, tags. FeedbackEvent: session_id, turn_id, rating, comment, tags, context_snapshot. SQLite storage. JSONL export. Opt-in only. TUI: rating prompt after response. VS Code: rating widget.
  Wave 7 | Blocked by: 24, 28 | Blocks: Wave 8
  References: Cursor Tab RL at .omo/research/agent-architectures.md data flywheel; Claude Code feedback
  Acceptance: feedback stored/retrievable; rating prompt appears; opt-out disables all; export valid JSONL
  QA: Happy: thumbs down + comment → stored with context; Failure: storage full → oldest pruned; Privacy: opt-out → zero data
  Commit: Y | feat(w7): implement feedback collection and rating system

### Wave 8 (Integration & Data Flywheel)

- [x] 32. Implement OpenTelemetry tracing and structured logging
  What: In `core/src/observability/`: Tracer — spans per turn/tool/model-request. Events: TurnStarted, ToolCallBegin/End, ModelRequestBegin/End, TokenUsage, CompactionTriggered. Structured logging via tracing crate. JSON lines output + OTLP exporter. Session metadata: model, turn_count, total_tokens, duration.
  Wave 8 | Blocked by: 5, 24, 28 | Blocks: None
  References: OpenTelemetry Rust SDK; Codex rollout recorder at openai/codex/codex-rs/core/src/lib.rs:150
  Acceptance: spans created; JSON output valid; OTLP exporter works; zero overhead when disabled
  QA: Happy: session trace visible in Jaeger; Failure: collector unreachable → file fallback; Perf: overhead <5%
  Commit: Y | feat(w8): implement OpenTelemetry tracing and logging

- [x] 33. Build failure clustering and data flywheel
  What: In `core/src/flywheel/`: FailureAnalyzer — cluster by error_type, tool_name, language, file_pattern, turn_count. ImprovementSuggester — for each cluster, suggest tool description/context strategy/prompt changes. Nightly pipeline: collect traces → cluster → suggest → (optional) auto-update tool descriptions.
  Wave 8 | Blocked by: 32 | Blocks: None
  References: JTPRO at .omo/research/agent-architectures.md:459; Fission-GRPO error recovery
  Acceptance: 100 errors clustered meaningfully; suggestions generated; pipeline runs without crash
  QA: Happy: "file not found" errors → cluster + tool description suggestion; Failure: no errors → empty report; Edge: single error → singleton cluster
  Commit: Y | feat(w8): build failure clustering and flywheel

- [x] 34. Build session persistence and resume capability
  What: In `core/src/persistence/`: SessionStore — SQLite schema: sessions, turns, tool_calls, events tables. SessionSerializer — full state serialization for resume. Auto-save after each turn. Resume: SessionStore::resume(id) -> Session. Auto-cleanup: archive >30 days.
  Wave 8 | Blocked by: 24 | Blocks: None
  References: Codex rollout persistence; Claude Code session state; SQLite best practices
  Acceptance: session persisted and resumed; auto-save after each turn; archive removes old; resume includes full history
  QA: Happy: crash → resume from last completed turn; Failure: corrupted SQLite → error + backup recovery; Bench: 1000 sessions <1s
  Commit: Y | feat(w8): implement session persistence and resume

- [x] 35. End-to-end integration testing and hardening
  What: Integration test suite: (1) complete coding task, (2) debug workflow, (3) code review, (4) multi-file refactor, (5) interruption+resume. Performance benchmarks: startup, turn latency, compaction, search. Stress: 50-turn session with large files. Security: verify sandbox, permissions, no data leakage.
  Wave 8 | Blocked by: 32, 33, 34 | Blocks: None
  References: Codex integration tests; .omo/research/toolchain-integration.md security
  Acceptance: 5 scenarios pass; startup <500ms; turn <5s p95; compaction <2s; search <1s; sandbox confirmed
  QA: Happy: full task prompt→commit <5 turns; Failure: invalid permission → blocked; Stress: 50-turn <64K tokens
  Commit: Y | feat(w8): end-to-end integration testing and hardening

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for user explicit okay before declaring complete.

- [x] F1. **Plan compliance audit** — Every todo's acceptance criteria verified against actual implementation; all 8 waves completed; scope IN fully delivered; scope OUT not violated. Evidence: .omo/evidence/f1-compliance-report.md
- [x] F2. **Code quality review** — `cargo clippy -- -D warnings` passes; `cargo audit` clean; no `unwrap()` in production code; all public items documented; test coverage >80% core engine, >60% tools. Evidence: .omo/evidence/f2-quality-report.md
- [x] F3. **Real manual QA** — Run agent on 3 real-world coding tasks (implement feature, fix bug, refactor); verify output correctness; verify TUI, exec mode, and VS Code extension all functional. Evidence: .omo/evidence/f3-qa-report.md
- [x] F4. **Scope fidelity** — No scope creep: no cloud-only, no SSO, no LLM training, no mobile. Architecture decisions followed: Rust core, MCP tools, graph-first indexing, local-first. Evidence: .omo/evidence/f4-scope-audit.md

## Commit strategy
- Per-todo commits: one commit per todo (or N/A for infra)
- Format: conventional commits `feat(w<N>):`, `fix(w<N>):`, `chore(w<N>):`, `docs:`, `test:`
- No force push; branch strategy: `w<N>/<short-desc>` → PR → main
- Wave merging: each wave merged to main after all todos verified
- GPG-sign all commits; changelog auto-generated from conventional commits

## Success criteria
1. `cargo build --release` produces binaries for Linux/macOS/Windows
2. `cargo test --workspace` passes all tests
3. `cargo clippy -- -D warnings` zero warnings
4. Agent completes 3 real-world coding tasks end-to-end
5. HumanEval pass@1 >70%; SWE-bench-Live resolve rate >25%
6. VS Code extension activates and communicates with App Server
7. TUI renders and handles user input correctly
8. Sandbox blocks dangerous commands (no escape)
9. All design Mermaid diagrams accurately represent implemented system
10. Final Verification Wave — all 4 reviewers APPROVE
