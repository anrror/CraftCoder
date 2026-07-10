# F4: Scope Fidelity Audit — AI Coding Agent

**Date**: 2026-07-09  
**Auditor**: Sisyphus-Junior  
**Project**: `E:\code\ai\y-ai-coding`  
**Status**: ⚠️ **CONDITIONAL APPROVE** (2 minor findings, 0 blockers)

---

## 1. No Cloud-Only Features

**Verdict**: ✅ PASS

**Evidence**:
- All Cargo.toml files audited (10 workspace members + 1 standalone eval-bridge)
- No cloud SDK dependencies found (`aws`, `gcp`, `azure`, `firebase`, `supabase`, `s3`, `lambda`, `dynamodb`)
- The only external API dependency is `reqwest` for HTTP calls to the Qwen3 API gateway — this is a **local-first** architecture that connects to a configurable API endpoint
- `rusqlite` (bundled) is used for local SQLite storage — no cloud database
- `opentelemetry` is optional (`otlp` feature) and only for observability export, not a core dependency
- The `remote-datasets` feature in `eval` is optional and only used for downloading benchmark datasets (SWE-bench, HumanEval) — not a cloud service dependency

**Finding**: The default `api_base_url` in `cli/src/config.rs:117` points to `https://api.openai.com/v1` — this is a **configurable default** that can be overridden. The actual model client (`core/src/model/config.rs:29`) defaults to `https://prod-ai.isigning.cn/v1` (Qwen3 gateway). The CLI default is misleading but not a cloud lock-in since it's user-overridable.

---

## 2. No SSO / User Management Code

**Verdict**: ✅ PASS

**Evidence**:
- Grep for `auth`, `login`, `password`, `sso`, `oauth`, `jwt`, `session` (as auth), `cookie`, `sign.?in`, `sign.?up`, `register` in all `.rs` files — **zero matches** for user authentication/management patterns
- The only "session" references are to **agent conversation sessions** (ReAct loop turns), not user login sessions
- No user database, no user roles, no permission system beyond tool allowlisting
- The VSCode extension has no auth flows — it connects to a local app server via JSON-RPC

---

## 3. No LLM Training Infrastructure

**Verdict**: ✅ PASS

**Evidence**:
- No training loops, no fine-tuning code, no dataset generation for model training
- The `eval/` module contains **benchmark evaluation** code (SWE-bench, HumanEval) — this is for **testing agent performance**, not for training models
- The `feedback/` module collects user ratings (thumbs up/down) — this is for **quality monitoring**, not training data pipelines
- No references to `train`, `training`, `fine.?tune`, `dataset` (in the training sense) in any Rust source
- The `eval` module downloads benchmark datasets from HuggingFace — these are **standardized evaluation datasets**, not training data

---

## 4. No Mobile App Code

**Verdict**: ✅ PASS

**Evidence**:
- Grep for `mobile`, `android`, `ios`, `swift`, `kotlin`, `react.?native`, `flutter` in all `.rs`, `.ts`, `.toml` files — **zero matches** in project source code
- The only matches are in `node_modules/` (third-party type definitions) — irrelevant
- The project has three client surfaces:
  1. **CLI** (Rust TUI via `ratatui` + `crossterm`) — desktop terminal
  2. **VSCode extension** (TypeScript) — desktop IDE
  3. **Web API** (Axum REST + SSE) — local web server, not a mobile app

---

## 5. Architecture Decisions

### 5a. Rust Core Engine ✅

**Verdict**: ✅ PASS

**Evidence**:
- All 10 workspace members are Rust crates
- Core engine in `code-agent-rs/core/` with agent loop, model client, context management
- Code indexing in `code-agent-rs/codex/` with tree-sitter parsing
- Tool system in `code-agent-rs/tools/` with git, LSP, MCP, shell
- Protocol types in `code-agent-rs/protocol/`
- CLI in `code-agent-rs/cli/` with ratatui TUI
- App server in `code-agent-rs/app-server/` for IDE integration
- Web API in `code-agent-rs/code-agent-web/` with Axum
- Eval bridge in `eval-bridge/` with PyO3 for Python interop

### 5b. MCP Tool Protocol ✅

**Verdict**: ✅ PASS

**Evidence**:
- `code-agent-rs/tools/src/mcp/` contains MCP client and server implementations
- `tools/Cargo.toml` has `mcp` feature flag (enabled by default)
- `tools/src/lib.rs` exports `pub mod mcp` behind `#[cfg(feature = "mcp")]`
- MCP is a first-class citizen in the tool system alongside LSP, git, and shell

### 5c. Graph-First Indexing ✅

**Verdict**: ✅ PASS

**Evidence**:
- `code-agent-rs/codex/src/graph/` contains `CallGraph`, `DependencyGraph`, `EdgeConfidence`, `ImpactResult`
- `code-agent-rs/codex/src/indexer/` contains `CodeIndexer`, `parser`, `storage`, `symbol`
- `code-agent-rs/codex/src/retrieval/` for search and retrieval
- `code-agent-rs/codex/src/watcher/` for file watching and index sync
- Uses tree-sitter for AST parsing across Python, TypeScript, Rust, Go, Java
- SQLite-backed storage with `rusqlite` (bundled)
- LRU cache (`lru` crate) for hot paths

### 5d. Local-First Architecture ✅

**Verdict**: ✅ PASS

**Evidence**:
- All data stored locally via SQLite (`rusqlite` with `bundled` feature)
- File watching via `notify` crate for local filesystem changes
- Git integration via `git2` (libgit2 bindings) — local git operations
- LSP integration for local language server communication
- The only network dependency is the model API call (Qwen3 gateway) — this is a **configurable endpoint**, not a cloud lock-in
- VSCode extension communicates with a **local** app server via JSON-RPC over stdio
- Web API is a **local** Axum server, not a cloud deployment

---

## 6. Qwen3 Models Used Throughout (Not OpenAI/Anthropic)

**Verdict**: ⚠️ **PASS with findings**

**Evidence**:
- **Core model client**: `core/src/model/qwen3.rs` — `Qwen3OpenAIClient` uses Qwen3 models via OpenAI-compatible API
- **Default models** in `core/src/model/config.rs`:
  - Chat: `Qwen3.6-27B-AEON-Ultimate-Uncensored-NVFP4`
  - Embedding: `Qwen3-Embedding-8B`
  - Reranker: `Qwen3-Reranker-8B`
  - Vision: `qwen3-vl`
  - Guard: `Qwen3Guard-Gen-8B`
- **API gateway**: `https://prod-ai.isigning.cn/v1` (Qwen3 gateway)
- **Environment variable**: `QWEN3_API_KEY` (not `OPENAI_API_KEY`)

**Finding 1 — Minor**: `cli/src/config.rs:117` has default `api_base_url: "https://api.openai.com/v1"` and test at line 460 uses `"gpt-4"`. This is a **stale default** in the CLI config layer. The actual model client in `core/` correctly defaults to Qwen3. The CLI default is misleading but overridable.

**Finding 2 — Minor**: `core/src/feedback/` tests use `"gpt-4"` as mock context snapshot values (lines 212, 223, 384, 394 in `mod.rs`, `export.rs`, `collector.rs`). These are **test fixtures** for the feedback data model, not actual model configuration. They should be updated to use Qwen3 model names for consistency.

---

## Summary

| Check | Status | Details |
|-------|--------|---------|
| 1. No cloud-only features | ✅ PASS | Local SQLite, no cloud SDKs |
| 2. No SSO/user management | ✅ PASS | No auth code found |
| 3. No LLM training infra | ✅ PASS | Eval only, no training |
| 4. No mobile app code | ✅ PASS | CLI + VSCode + Web only |
| 5a. Rust core engine | ✅ PASS | 10 Rust workspace members |
| 5b. MCP tool protocol | ✅ PASS | Full MCP client/server impl |
| 5c. Graph-first indexing | ✅ PASS | CallGraph + CodeIndexer + tree-sitter |
| 5d. Local-first architecture | ✅ PASS | SQLite, local git, local LSP |
| 6. Qwen3 models throughout | ⚠️ PASS | 2 minor findings (stale defaults in CLI config + test fixtures) |

## Final Verdict: **CONDITIONAL APPROVE**

**No blockers found.** The architecture is sound and all scope guardrails are respected.

**Recommended remediation (non-blocking)**:
1. Update `cli/src/config.rs:117` default `api_base_url` from `"https://api.openai.com/v1"` to `"https://prod-ai.isigning.cn/v1"` to match the core config
2. Update `cli/src/config.rs` test at line 460 from `"gpt-4"` to `"qwen3.6-27b"`
3. Update `core/src/feedback/` test fixtures from `"gpt-4"` to `"Qwen3.6-27B-AEON-Ultimate-Uncensored-NVFP4"`
