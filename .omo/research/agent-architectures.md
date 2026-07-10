# AI Coding Agent Architectures: A Comprehensive Research Report

**Date**: July 2026
**Scope**: Publicly available information from papers, blog posts, official docs, and reputable analysis
**Focus**: Deep technical architecture of production AI coding agents

---

## Table of Contents

1. [ReAct Loop Pattern](#1-react-loop-pattern)
2. [Agent Framework Comparison](#2-agent-framework-comparison)
3. [Task Decomposition Strategies](#3-task-decomposition-strategies)
4. [Context Management](#4-context-management)
5. [Code Generation Techniques](#5-code-generation-techniques)
6. [Observability & Data Flywheel](#6-observability--data-flywheel)
7. [Tool-Use Protocols](#7-tool-use-protocols)
8. [References](#8-references)

---

## 1. ReAct Loop Pattern

### 1.1 The Core Mechanism

The ReAct (Reasoning + Acting) pattern, introduced by Yao et al. at ICLR 2023 (arXiv:2210.03629), is the foundational control primitive for virtually all modern AI coding agents. The loop interleaves three phases:

```
Reason (Thought) -> Act (Tool Call) -> Observe (Tool Result) -> Reason -> ...
```

Each iteration:
1. **Thought**: The model emits free-form reasoning over current context
2. **Act**: A typed tool call from the available action space (read, edit, bash, grep, etc.)
3. **Observation**: The tool's return value, appended verbatim to context

The loop terminates when the model emits a final answer instead of a tool call.

### 1.2 Convergence Across Production Systems

As documented by Daniel Vaughan (Apr 2026), an academic taxonomy of 13 coding agent source codes confirmed that 7 of 13 use sequential ReAct as their core control primitive, and the remaining 6 use variations following the same observe-act-reflect cycle. The pipeline shared by Codex CLI, Claude Code, Cursor, Copilot Agent, Windsurf, Jules, and others:

\\\
Receive & Contextualize -> Plan -> Act via Tool Calls -> Observe Results -> Task Complete? -> (loop or return)
\\\

**Source**: [The Great Convergence: Why Every AI Coding Agent Now Runs the Same Pipeline](https://codex.danielvaughan.com/2026/04/15/coding-agent-pipeline-convergence/)

### 1.3 Claude Code (Anthropic) — Reference Implementation

The most thoroughly documented production ReAct implementation. Key architectural details from source-code analysis (Liu et al., arXiv:2604.14228, Apr 2026):

**Core Loop Structure**:
- \queryLoop()\ async generator in \query.ts\ implements the iterative agent loop
- A \QueryEngine\ session object holds long-lived state across entire conversations
- State object includes: \messages\, \	oolUseContext\, \	urnCount\, \shouldAutoCompact\, \utoCompactTracking\, \borted\

**Pseudocode** (from community source analysis):
\\\	ypescript
while (!state.aborted) {
  const query = buildQuery(state)
  const response = await requestModelAPI(query)
  const parsed = parseModelResponse(response)
  if (!parsed.hasToolUse) return parsed.finalAnswer
  const toolResults = await runTools(parsed.toolUses, state.toolUseContext)
  state = appendToolResultsToMessages(state, response, toolResults)
  state = maybeAutoCompact(state)
  state = nextTurn(state)
}
\\\

**Key Insight**: Community analysis estimates only ~1.6% of Claude Code's codebase constitutes AI decision logic. The remaining 98.4% is operational infrastructure — safety, context management, tool routing, persistence.

**Source**: [Dive into Claude Code](https://arxiv.org/html/2604.14228v1)

### 1.4 Codex CLI (OpenAI)

Codex CLI encodes the ReAct cycle through its Thread -> Turn -> Item hierarchy:
- **Thread**: A full session
- **Turn**: One complete iteration of the loop (model call -> tool execution -> result)
- **Item**: An atomic action within a turn

Codex CLI uses a two-phase extraction-consolidation pipeline backed by SQLite for context management, and supports up to 6 parallel cloud-exec workers.

### 1.5 Cursor

Cursor operates as an IDE-embedded agent using the same ReAct loop but optimized for real-time interaction. It supports multi-model backends (GPT-5.3, Sonnet 4.5, Gemini 3 Pro) and uses tree-sitter + PageRank-based repository maps pioneered by Aider.

### 1.6 GitHub Copilot Agent

Copilot Agent evolved from autocomplete (FIM paradigm) to a full ReAct agent loop. It follows the convergent pipeline with configurable permission modes.

### 1.7 Devin (Cognition)

Devin is an outlier that invests more heavily in scaffolding-side reasoning. Unlike Claude Code's minimal-scaffolding approach, Devin maintains explicit planning and task-tracking structures, using a Planner -> Worker -> Judge hierarchical pattern.

### 1.8 When ReAct Fails

Research identifies three predictable failure modes:
1. **Hallucinated tool names**: In a 200-task benchmark, 90.8% of retries targeted hallucinated tool names
2. **Infinite action loops**: The model calls the same tool with minor variations
3. **Quadratic token cost**: Each iteration re-prefills the entire history

**Mitigations**: Profile-Then-Reason (PTR) bounds LM calls to 2-3 per task and beats ReAct on 16 of 24 configurations. Deterministic tool routing (output a step type, not a tool name) fixes hallucinated tool issues.

---

## 2. Agent Framework Comparison

### 2.1 MCP (Model Context Protocol)

| Attribute | Detail |
|-----------|--------|
| Creator | Anthropic (late 2024), now Linux Foundation Agentic AI Foundation |
| Focus | Tool/resource access protocol (agent-to-tool layer) |
| Transport | JSON-RPC 2.0 over stdio, SSE, Streamable HTTP |
| Adoption | 97M+ monthly SDK downloads, 10,000+ public servers, 800+ in official registry (May 2026) |
| Framework Support | LangChain, LlamaIndex, CrewAI, Vercel AI SDK, PydanticAI, Mastra, Spring AI, MS Agent Framework, DSPy |

**Strengths**:
- Universal tool adapter: build once, use from any MCP-compatible client
- Clean separation: "tools are servers, agents are clients"
- Mature ecosystem with stable 1.0 protocol, OAuth 2.1 enforcement (Jul 2026 RC)
- W3C Trace Context for observability

**Weaknesses**:
- No built-in agent orchestration or state management
- Stateless between calls — external memory layer required
- No native support for agent-to-agent delegation

**When to use**: Single-agent tool access, or as the universal tool layer in any multi-agent stack.

### 2.2 A2A (Agent-to-Agent Protocol)

| Attribute | Detail |
|-----------|--------|
| Creator | Google (Apr 2025), now Linux Foundation Agentic AI Foundation |
| Focus | Agent-to-agent delegation and communication |
| Transport | HTTP + Server-Sent Events |
| Adoption | v1.0, production in 150+ organizations (Apr 2026) |

**Strengths**:
- True delegation: Agent B is a reasoning system, not a passive tool
- Cross-organization: works across company boundaries
- Agent Card capability discovery
- Graceful degradation and auditability

**Weaknesses**:
- Adds network round-trips (not for sub-second latency)
- Designed for isolation, not tight state sharing
- Slower adoption than MCP

**When to use**: Multi-agent collaboration across organizational boundaries, specialist agent delegation.

### 2.3 LangChain / LangGraph

| Attribute | Detail |
|-----------|--------|
| Creator | LangChain (private company) |
| Focus | Orchestration framework (agent loops, state management) |
| Adoption | 1B+ cumulative downloads, 1M+ practitioners, 300+ LangSmith enterprise customers |

**Strengths**:
- Largest ecosystem with 300+ integrations
- Explicit control flow via StateGraph with typed edges
- Human-in-the-loop checkpoints, time-travel debugging
- LangSmith observability platform (15B+ agent traces)

**Weaknesses**:
- Over-abstracted — simple tasks require understanding chains, runnables, callbacks
- Breaking changes across minor versions
- Performance overhead from abstraction layers
- Vendor lock-in risk (framework, not protocol)

**When to use**: Complex multi-step workflows with branching, state persistence, and human oversight.

### 2.4 LlamaIndex

| Attribute | Detail |
|-----------|--------|
| Creator | Jerry Liu (2022) |
| Focus | RAG-first framework, data indexing and retrieval |
| Language | Python, TypeScript |
| GitHub Stars | 41k |

**Strengths**:
- Best-in-class RAG with 70+ loaders, multi-tier indexes
- GraphRAG (entity graph + embeddings) — smoothest in ecosystem
- Event-driven Workflow abstraction
- Bidirectional MCP (consume and expose)

**Weaknesses**:
- Not a general agent framework (no native tool calling, orchestration)
- Narrowly focused on retrieval
- Python-only core

**When to use**: Document-heavy RAG workloads, knowledge assistants, data pipelines.

### 2.5 DIY / Custom Approaches

**Strengths**: Complete control over the loop, minimal dependencies, maximum iteration speed.
**Weaknesses**: Must build context management, error recovery, observability from scratch. No MCP/A2A interop without custom implementation.
**Best for**: 1-2 simple agents, zero-dependency preference.

### 2.6 Convergence Point

The industry consensus (as of mid-2026) is that these are complementary layers, not alternatives:

\\\
LangGraph         -> Orchestration (decides what to do)
MCP               -> Tool access (how to do it)
A2A               -> Agent delegation (who to ask)
\\\

Most production systems combine all three: LangGraph for orchestration + MCP for tools + A2A for cross-agent work.

---

## 3. Task Decomposition Strategies

### 3.1 Chain-of-Thought Decomposition (CoT)

The simplest and most widely-used technique. The model produces an explicit step list before executing any action.

**Production pattern**: Add \success_criteria\ and \isks\ fields to each step for structured output. Build a replanning node that fires when a step fails.

**Cost**: 1 LLM call per plan
**Best for**: Most tasks — the default starting point

### 3.2 Plan-and-Execute (P-t-E)

The agent generates a complete, frozen plan first, then executes steps in sequence. The planner is insulated from execution outputs.

**Variants**:
- **Sequential decomposition**: Ordered list of steps with strict dependencies
- **DAG decomposition**: Nodes in a dependency graph, enabling parallel execution
- **Hierarchical decomposition (HTN)**: Tree of goals/sub-goals recursively refined
- **Dynamic decomposition**: Task list as a live data structure (BabyAGI pattern)

**Production implementations**:
- Claude Code Task tool spawns subagents with isolated context windows
- Codex CLI dispatches up to 6 parallel cloud workers
- Roo Code uses mode-based routing (Architect -> Code -> Debug)

### 3.3 Tree-of-Thought (ToT)

Extends CoT by generating multiple candidate plans, evaluating each, and selecting the best one. Each node is an intermediate reasoning state.

**Cost**: 4-6x LLM calls per planning step
**Best for**: High-stakes irreversible tasks (sending emails, modifying production data, financial transactions)

**Key insight**: The evaluation step is where most implementations go wrong. Better evaluators check structural properties rather than abstract quality.

### 3.4 Monte Carlo Tree Search (MCTS) for Coding

MCTS adapts the game-tree search algorithm for agent planning. Four phases:

1. **Selection**: Choose which part of the plan tree to expand (UCB/UCT formula)
2. **Expansion**: Generate new candidate next steps
3. **Simulation/Rollout**: Estimate value via fast LLM simulation
4. **Backpropagation**: Update node value estimates

**Research implementations**:

**ToolTree** (ICLR 2026): Applies MCTS to tool planning with dual-feedback (pre-evaluation predicts utility before execution, post-evaluation scores actual output). Bidirectional pruning eliminates unpromising branches. Achieves ~10% average improvement over SoTA. [Source](https://arxiv.org/pdf/2603.12740)

**Reason-Code** (ACL 2026 Industry): Formulates code generation as MDP-guided search with MCTS. Uses conditional budgeting — search activates only when greedy generation fails. Matches Best-of-10 sampling with lower token cost. [Source](https://aclanthology.org/2026.acl-industry.30.pdf)

**SGA-MCTS** (ACL 2026 Findings): Casts LLM planning as non-parametric retrieval. MCTS mines optimal paths offline and distills them into de-lexicalized State-Goal-Action atoms for online retrieval. Matches GPT-5 without task-specific fine-tuning. [Source](https://aclanthology.org/2026.findings-acl.60.pdf)

### 3.5 Planning Comparison Matrix

| Technique | LLM Calls/Plan | Parallelism | Best For | Main Cost |
|-----------|---------------|-------------|----------|-----------|
| Chain-of-Thought | 1 | None | Most tasks, baseline | Single-plan rigidity |
| Tree-of-Thought | 4-6 | Plan gen only | High-stakes irreversible | 4-6x LLM cost |
| Task Graph (DAG) | 1 + execution | Full parallel | Research, data gathering | DAG runtime complexity |
| MCTS | 50-200 | Simulation phase | Large action spaces | Very high LLM cost |

### 3.6 Industry Convergence

Mike Mason (Jan 2026) documented how Claude Code, Cursor, Devin, OpenHands, and Aider all converged on hierarchical orchestration with Planners, Workers, and Judges — after initial peer-agent approaches proved inefficient.

**Standard pattern**:
1. A **planner** decomposes the task
2. **Workers** execute subtasks (often in parallel, often in isolated git worktrees)
3. A **reviewer/judge** validates the output

---

## 4. Context Management

### 4.1 The Binding Constraint

Claude Code's architecture treats the context window as the binding resource constraint. With 200K tokens (older models) to 1M tokens (Claude 4.6 series), the overhead accumulates rapidly:

| Component | Typical Tokens |
|-----------|---------------|
| System prompt | ~10K |
| Tool definitions | ~5K |
| User messages | ~200-2K |
| Assistant reasoning | ~500-2K per turn |
| Tool calls | ~100-500 each |
| Tool results | ~500-10K+ each |

After 20-30 tool calls, most agents approach the ceiling.

### 4.2 Claude Code's 5-Layer Compaction Pipeline

Before every model call, five sequential shapers manage context pressure:

1. **Budget Reduction**: Targets individual tool outputs that overflow size limits
2. **Snip**: Handles temporal depth — removes or trims deep conversation history
3. **Microcompact**: Reacts to cache overhead pressure
4. **Context Collapse**: Manages very long histories by aggregating
5. **Auto-Compact**: Semantic compression as a last resort — LLM summarizes old turns

### 4.3 Research Advances

**CAT — Context as a Tool** (ACL 2026 Findings):
Elevates context management to a callable tool. SWE-Compressor achieves 57.6% solved rate on SWE-Bench Verified, significantly outperforming append-only ReAct. Maintains stable ~35K token usage even across 500 interaction rounds, while ReAct degrades after ~60 rounds. [Source](https://aclanthology.org/2026.findings-acl.1032.pdf)

**SWE-MeM** (arXiv, Jun 2026):
Training framework for proactive memory management. Achieves 60.2% resolve rate with Qwen3-Coder-30B under 32K context budget. Uses Memory-aware GRPO for joint optimization. [Source](https://arxiv.org/html/2606.28434)

**ACON — Agent Context Optimization** (2025):
Reframes compression as optimization problem. 26-54% peak token reduction while maintaining task accuracy. Smaller models (Qwen3-14B) approach larger model performance. Observation masking (replacing old tool outputs with placeholders) often outperforms LLM summarization at a fraction of the cost.

### 4.4 Production Compression Strategies

**Content Value Hierarchy**:
- **Preserved**: System prompt, user instructions, recent edits, in-progress modifications
- **Summarized**: Older assistant reasoning, intermediate search results
- **Evicted**: Large grep outputs already acted upon, file reads of unedited files, verbose command output

**The "Lost in the Middle" Problem**: LLMs show U-shaped attention — content at the beginning and end receives strong attention, while middle content gets less. Middle content is safest to compress.

**Budget thresholds**: 70% — monitor; 85% — trigger compression; 95% — aggressive eviction.

### 4.5 Advanced Techniques

**Observation Masking**: Replace old tool outputs with placeholders — outperforms LLM summarization at fraction of cost (JetBrains, 500 SWE-bench instances).

**Subagent Isolation**: The dominant 2025-2026 pattern. Coordinator passes only relevant information; subagents operate in clean context windows. Token consumption drops ~67% compared to skills-based approaches.

**Anthropic's Compaction API**: \compact-2026-01-12\ header on Claude models. Server-side aging-context summarization. Custom \instructions\ parameter preserves domain-specific artifacts.

**Context Budget Engineering** (exemplary allocation):
| Segment | Budget | Policy |
|---------|--------|--------|
| System + tools | ~4K | Static, cached |
| Task + hypothesis | ~1K | Always resident |
| Retrieved docs | ~8K | Top-5 reranked |
| Tool results | ~10K | Just-in-time, evicted after action |
| Working memory | ~6K | Compacted past threshold |
| Model reply headroom | ~8K | Reserved |

---

## 5. Code Generation Techniques

### 5.1 Edit Format Taxonomy

**Full-code generation**: Write entire file from scratch. Reliable but token-inefficient.

**Search-and-Replace**: Anchor edits on semantic content rather than line numbers. Used by Claude Code (\str_replace_editor\), Aider, and most agentic systems.

**Unified Diff (patch)**: Compact but fragile — depends on exact line numbers. LLMs frequently produce invalid patches.

**Whole-file rewrite**: Robust for complex restructuring but costly for long files.

### 5.2 Research Advances

**BlockDiff / FuncDiff** (ACL 2026 Findings):
Structure-aware diff formats representing changes as block-level rewrites of syntactically coherent units (control structures, functions). **AdaEdit** trains LLMs to dynamically choose between diff and full-code formats. Reduces latency and cost by 30%+ on long-code editing. [Source](https://aclanthology.org/2026.findings-acl.1483/)

**Search-and-Replace Infilling (SRI)** (ACL 2026 Long):
Transforms FIM completion from static text continuation to intelligent micro-editing. First generates SEARCH block (grounding mechanism), then REPLACE block. Bridges security alignment gap — Chat models inherit safety features. [Source](https://aclanthology.org/2026.acl-long.361.pdf)

**SWE-Edit** (Microsoft, Apr 2026):
Decomposes editing into Viewer + Editor subagents. Adaptive mode selection (find-replace vs. whole-file) trained via GRPO on Qwen3-8B achieves 12.5pp edit success improvement — 8B model reaches parity with GPT-5nano. [Source](https://arxiv.org/pdf/2604.26102v2)

**CODESTRUCT** (ACL 2026):
Reframes codebase as structured action space on AST entities. \eadCode\ retrieves syntactic units; \editCode\ applies AST-validated transformations. Improves Pass@1 by 1.2-5.0%, cuts tokens 12-38%. GPT-5-nano improves 20.8% as empty-patch failures drop from 46.6% to 7.2%. [Source](https://aclanthology.org/2026.acl-long.607.pdf)

**aiXapply-4B** (2026):
Specialized 4B model for full-file apply. 94.4% equivalence accuracy. 1.06s average latency on single A100 via n-gram speculative decoding.

### 5.3 Multi-File Coordination

**InlineCoder** (arXiv, Jan 2026):
Repository-level code generation via context inlining. Generates draft "anchor," then bidirectional inlining (upstream callers + downstream callees). 29.73% EM improvement on RepoExec. [Source](https://arxiv.org/html/2601.00376v2)

**Hydra Retriever** (arXiv, Feb 2026):
Combines dependency-aware retrieval (DAR) with BM25 similarity search. Structure-aware indexing preserves AST-level relationships. Lowest and most stable latency by anchoring on call-graph dependencies.

### 5.4 Streaming Generation

Modern agents support streaming code generation. The harness streams tokens as the model generates, enabling real-time visibility into edits. Key challenges: maintaining edit format boundaries during streaming, handling partial tool calls, and coordinating streaming across multi-file edits.

---

## 6. Observability & Data Flywheel

### 6.1 The Observability-Evaluation Gap

The arXiv 2604.14228 analysis identifies "silent failure and the observability-evaluation gap" as a key open direction. Production coding agents can produce apparently correct output that is semantically wrong, and current observability infrastructure is not equipped to detect this.

### 6.2 Tracing Infrastructure

**OpenTelemetry (OTel)**:
- W3C Trace Context is now mandatory in MCP 2026-07-28 RC for audit trails per invocation
- Frameworks like LangSmith (15B+ traces), Langfuse, and Helicone provide first-class agent tracing
- Minimum viable trace schema: \session_id\, \	urn\, \	ool_name\, \	ool_use_id\, \input\, \output\, \latency_ms\, \success\, \error_type\

**LangSmith**: LangChain's observability platform with 300+ enterprise customers, 15B+ agent traces, time-travel debugging for LangGraph agents, run replay.

**Logfire** (PydanticAI): OTel-native observability for type-safe Python agents with Pydantic schema validation.

### 6.3 Step-Level Logging Patterns

**Production recommendations**:
- Log every tool call with: input arguments, output, latency, turn number, session ID
- Track \udget_evictions\ per session for tuning
- Log token usage per layer: system, tools, RAG, history, current turn
- Archive full traces externally for audit even when model sees summaries
- Monitor token trends per completed task — rising averages indicate accumulation without quality improvement

### 6.4 Human-Feedback Loops

**Trust Trajectories**: Claude Code's longitudinal data shows auto-approve rates increase from ~20% at <50 sessions to >40% by 750 sessions.

**Permission approval patterns**: Users approve 93% of permission prompts in auto-mode — leading to architectural restructuring toward defined boundaries rather than per-action approvals.

**PostToolUse hooks**: Claude Code's 27 hook events include \StopFailure\ for observability (fires on API error termination), enabling harnesses to log failures and feed external recovery workflows.

### 6.5 RLHF for Code Generation

**Fission-GRPO** (ACL 2026): Converts execution errors into on-policy corrective supervision. Improves error recovery rate by 5.7% on BFCL v4 Multi-Turn and overall accuracy by 4.0%. [Source](https://aclanthology.org/2026.acl-long.1880/)

**Memory-aware GRPO** (SWE-MeM): Jointly optimizes memory management and issue resolution through memory-aware trajectory splitting and step-level credit assignment.

**GRPO for editing** (SWE-Edit): Trains editing mode selection (find-replace vs. whole-file) via GRPO. 12.5pp improvement in edit success on Qwen3-8B.

### 6.6 Data Flywheel Architecture

The emerging data flywheel for coding agents:

\\\
1. Execution traces collected at runtime
2. Success/failure labeled (execution results, tests, human feedback)
3. Failure patterns clustered and diagnosed
4. Targeted improvements to tool descriptions (JTPRO), context mgmt (SWE-MeM), edit formats (SWE-Edit), error recovery (Fission-GRPO)
5. Updated agent deployed -> more traces collected
\\\

**JTPRO** (ACL 2026 Findings): Joint Tool-Prompt Reflective Optimization iteratively updates global instructions and per-tool schemas from labeled traces. Pareto-style candidate selection preserves diverse effective behaviors. 5-20% relative improvement on overall success rate. [Source](https://aclanthology.org/2026.findings-acl.2017.pdf)

---

## 7. Tool-Use Protocols

### 7.1 The Tool Calling Mechanism

The protocol is universal across providers:

1. Send system prompt + user message + JSON schema of available tools
2. Model responds with text or \	ool_use\/\	ool_calls\ block (structured JSON)
3. Application code executes the actual function
4. Result sent back as \	ool_result\ message
5. Model resumes — either calls another tool or generates final response

### 7.2 Tool Schema Design

**Critical quality factors**:
- **Precise descriptions**: Explain *when* to use the tool, not just what it does. Include scope, conditions, negative guidance.
- **Single responsibility**: One tool, one purpose — no \mode\ parameters
- **Explicit types**: Use enums for categoricals, specify format requirements
- **Clear output contract**: What the tool returns and what partial/empty results look like

**JTPRO findings**: Joint optimization of tool schemas + global instructions is more effective than either in isolation. Recurring cross-tool slot semantics should be globalized into instructions; tool-specific disambiguation stays local.

### 7.3 Tool Selection Strategies

| Mode | Behavior | Use Case |
|------|----------|----------|
| \uto\ | Model decides whether to call tools | Most agents |
| \equired\ | Model must call some tool | Structured extraction |
| \
one\ | Model must not call tools | Simple Q&A |
| Force-specific | Must call named tool | Data extraction pipelines |

**Dynamic tool loading**: Retrieve semantically relevant subset per task via vector similarity. Filesystem-based tool discovery reduces token overhead by up to 98%.

### 7.4 Parallel Tool Execution

All major frontier models (Claude 3.5+, GPT-4o, Gemini 1.5 Pro+) support parallel tool calls in a single turn.

**Dependency detection**: If Tool B needs Tool A's output -> sequential. If both callable with known inputs -> parallel.

**Challenges**: Parallel calls compete for rate limits, connection pools, and auth tokens simultaneously. Output merging with conflict resolution needed.

### 7.5 Error Handling

**Structured error format**: \{"error": "rate_limited", "retry_after": 30}\

**Best practices**:
- Always return a result, even for failures
- Make errors machine-parsable
- Tool layer absorbs transient failures (exponential backoff)
- Circuit breakers for persistent failures
- Max iteration caps (10 default, 20-30 for complex agents)

### 7.6 MCP as Universal Tool Protocol

MCP has become the de-facto standard for tool integration (97M+ monthly SDK downloads, 10,000+ public servers, supported by every major framework).

**Tool granularity design**: Prefer fewer, higher-level tools that match how agents reason. Use toolset agentization — group frequently co-used tools into specialized sub-agents.

### 7.7 Safety and Authorization

**Claude Code** (7 permission modes):
- Deny-first with human escalation
- ML-based auto-mode classifier with two-stage fast-filter + chain-of-thought evaluation
- PreToolUse hooks for deterministic interception
- Shell sandboxing

**Codex CLI**: OS kernel-level sandboxing (Seatbelt, Landlock, seccomp, bubblewrap)

**Cline/Roo Code**: Per-action approval for every file change and terminal command

**Converged safety spectrum**:
- Strict (approve every action): Cline, Roo Code
- Configurable (approve consequential): Claude Code, Codex CLI, Copilot, Windsurf
- Autonomous (approve nothing): CI/CD mode, bypassPermissions

---

## 8. References

### Papers
1. Yao et al., "ReAct: Synergizing Reasoning and Acting in Language Models," ICLR 2023. arXiv:2210.03629
2. Liu et al., "Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems," arXiv:2604.14228, Apr 2026
3. Sumers et al., "Cognitive Architectures for Language Agents," arXiv:2309.02427 (CoALA)
4. Shuo Yang et al., "ToolTree: Efficient LLM Agent Tool Planning via Dual-Feedback MCTS," ICLR 2026. arXiv:2603.12740
5. "SGA-MCTS: Decoupling Planning from Execution via Training-Free Atomic Experience Retrieval," ACL 2026 Findings
6. "Reason-Code: Reliable Code Generation via Test-Driven MCTS," ACL 2026 Industry
7. "Context as a Tool: Context Management for Long-Horizon SWE-Agents," ACL 2026 Findings
8. "SWE-MeM: Learning Adaptive Memory Management for Long-Horizon Coding Agents," arXiv:2606.28434, Jun 2026
9. Cheng et al., "To Diff or Not to Diff? Structure-Aware and Adaptive Output Formats," ACL 2026 Findings
10. "From Completion to Editing: Search-and-Replace Instruction Tuning," ACL 2026 Long
11. Zhang et al., "SWE-Edit: Rethinking Code Editing for Efficient SWE-Agent," arXiv:2604.26102, Apr 2026
12. "CODESTRUCT: Code Agents over Structured Action Spaces," ACL 2026
13. "InlineCoder: Repository-Level Code Generation via Context Inlining," arXiv:2601.00376, Jan 2026
14. "Do Not Treat Code as Natural Language: Repository-Level Code Generation," arXiv:2602.11671, Feb 2026
15. "The Evolution of Tool Use in LLM Agents: From Single-Tool Call to Multi-Tool Orchestration," arXiv:2603.22862, Apr 2026
16. Zhang et al., "Robust Tool Use via Fission-GRPO," ACL 2026
17. "JTPRO: A Joint Tool-Prompt Reflective Optimization Framework," ACL 2026 Findings
18. "DeepPlanning: Benchmarking Long-Horizon Agentic Planning," arXiv:2601.18137, Jan 2026
19. "Why Reasoning Fails to Plan," arXiv:2601.22311, Jan 2026
20. "GoalAct: Enhancing LLM-Based Agents via Global Planning and Hierarchical Execution," 2025

### Blog Posts & Articles
1. Daniel Vaughan, "The Great Convergence: Why Every AI Coding Agent Now Runs the Same Pipeline," Apr 2026
2. "Claude Code Source Analysis Series, Chapter 2: The ReAct Main Loop," DEV Community, May 2026
3. Mick Delaney, "Inside the ReAct Loop: How Agents Actually Iterate," Apr 2026
4. "Implement the ReAct Pattern in 50 Lines of Python," Growth Engineer, May 2026
5. "Multi-Agent Orchestration 2026: MCP vs A2A vs LangGraph," IoT Digital Twin PLM, Apr 2026
6. "AI Agent Frameworks 2026 Deep Dive," Chaos and Order, May 2026
7. "MCP and AI Frameworks Integration," ChatForest, Mar 2026
8. "Context Window Engineering," Systems Explained, Jun 2026
9. "LLM Agent Context Budget and Token Management Explained," Solana Garden, Jun 2026
10. "Context Engineering: The Runtime Discipline Behind Production AI Agents," Zylos Research, Apr 2026
11. "Context Engineering: The Window is a Budget," cloudandsre.com, Jun 2026
12. "LLM Tool Calling 2026: Build Reliable Function-Calling Agents," DeepFounder, Apr 2026
13. "Tool Engineering: Designing AI Agent Tooling," AgentPatterns.ai, 2026
14. "The Roadmap to Mastering Tool Calling in AI Agents," Machine Learning Mastery, May 2026
15. "AI Agent Goal Decomposition and Hierarchical Planning," Zylos Research, Mar 2026
16. "Agentic AI Architecture for Enterprises: MCP, A2A & Platform Stack," NeuralCoreTech, Apr 2026
17. "Surgical Dissections of 15 Production AI Coding Agents," AISignal, Apr 2026
18. Fabian Hertwig, "Code Surgery: How AI Assistants Make Precise Edits to Your Files," 2025

### Official Documentation & Repos
- Claude Code Official Docs: https://code.claude.com/docs/
- Anthropic Building Effective Agents: https://docs.anthropic.com/en/docs/agents-and-tools
- MCP Specification: https://modelcontextprotocol.io/
- A2A Protocol: https://github.com/google/A2A
- LangChain / LangGraph: https://github.com/langchain-ai/langgraph
- LlamaIndex: https://github.com/run-llama/llama_index
- Vercel AI SDK Tool Calling: https://ai-sdk.dev/docs/ai-sdk-core/tools-and-tool-calling
- aiXapply-4B: https://github.com/aixcoder-plugin/aiXapply-4B
- Dive into Claude Code: https://github.com/VILA-Lab/Dive-into-Claude-Code
- ToolTree: https://github.com/SYang2000/ICLR_2026_ToolTree
- SWE-Edit: https://github.com/microsoft/SWE-Edit
- Forgeplan: https://github.com/sushaan-k/forgeplan

---

*This report was compiled from publicly available sources as of July 2026. All claims are cited to their original sources. Internal details of proprietary systems are based on published source-code analysis, official documentation, and reputable third-party analysis.*
