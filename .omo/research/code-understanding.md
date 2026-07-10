# Code Understanding & Search for AI Coding Agents
 
**State-of-the-Art Research Report — July 2026**

> Compiled from academic papers, open-source tools, production systems, and empirical evaluations across the AI coding agent ecosystem.

---

## Table of Contents

1. [Code Indexing Strategies](#1-code-indexing-strategies)
2. [AST-based Analysis](#2-ast-based-analysis)
3. [Call Graph Construction](#3-call-graph-construction)
4. [Vector Databases for Code](#4-vector-databases-for-code)
5. [Dependency Analysis](#5-dependency-analysis)
6. [Cross-language Understanding](#6-cross-language-understanding)
7. [Context Retrieval for Agents](#7-context-retrieval-for-agents)
8. [Synthesis & Recommendations](#8-synthesis--recommendations)
9. [References](#9-references)

---

## 1. Code Indexing Strategies

### 1.1 The Indexing Landscape

Code indexing pre-organizes a codebase so AI agents can find relevant code without reading every file. Without indexing, agents explore file by file, wasting 60-70% of tokens on irrelevant reads. Indexing acts as a catalog, reducing token costs by 50-70% and improving output accuracy [vexp, 2026].

Three fundamental paradigms exist, each with distinct trade-offs:

| Dimension | Full-Index (Pre-computed) | On-Demand (Agentic) | Hybrid |
|-----------|--------------------------|---------------------|--------|
| Initial setup | 1-15 min indexing | None (instant start) | 1-15 min indexing |
| Query latency | 10-200ms | 5-30s (iterative) | 10-500ms |
| Freshness | Lag (snapshot-based) | Always current | Near-real-time |
| Token efficiency | High (50-70% savings) | Low (60-70% waste) | High |
| Infrastructure | Vector DB / graph DB | None needed | Both |
| Scale behavior | Degrades past ~50K lines (embeddings) | Consistent but slow | Consistent (w/ graphs) |

### 1.2 Production System Architectures

**Cursor** — Hybrid Semantic-Lexical (Embeddings + Grep)
- Background indexing creates embeddings for each workspace file
- Indexing time: 1-15 minutes depending on project size
- Two tools: "Codebase" (semantic search against pre-indexed embeddings) and "Grep" (exact keyword matching)
- Observed behavior tracks: semantic search → grep → selective reads

**Aider** — Graph-based Static Analysis (RepoMap)
- Symbol-level repository map built from AST analysis
- Achieves highest documented efficiency: 4.3-6.5% context utilization
- Deterministic, explainable, offline-capable (no GPU needed)
- Limitation: files with conceptual similarity but no explicit dependencies won't connect

**Claude Code** — Agentic (Tool-first Lexical)
- grep (pattern matching) + glob (file discovery) + read (whole file access)
- Whole-file reading when accessing files — complete context but higher token consumption
- Sub-agent delegation for multi-step searches

**Cline** — Three-Tier Agentic (ripgrep + fzf + Tree-sitter AST)
- ~17.5% context utilization, 300-result limit
- Multi-language AST parsing with efficient 35K token context

**FastCode** — Scout-First Semantic-Structural
- Multi-grained hybrid indexing across 4 levels: File, Class, Function, Documentation
- Lightweight metadata (type signatures, docstrings, line ranges)
- Sparse (BM25) + Dense (embedding) dual index
- Graph extension: Call, Dependency, and Inheritance graphs

### 1.3 Key Empirical Findings

- **No clear performance advantage** emerged for semantic search over lexical approaches — token consumption varies by 10× (8.5K to 117K tokens) with identical task success [Preprints.org, 2025]
- **Graph-based AST ranking (Aider) achieved the lowest resource consumption**
- **22% faster responses** with indexing, but **synchronization drift** caused phantom API calls [ForgeCode, 2025]
- **AOCI** produced zero final-state defects on 19 industrial tasks while mainstream tools introduced defects in 12 tasks at 4-130× higher token cost [AOCI, 2026]

### 1.4 The Sync Problem

Code indexes are snapshots. When the codebase changes:
- Phantom Dependencies, API Drift, Deprecated Patterns, Dead Code Suggestions
- Solutions: incremental indexing with content-hash detection, file-system watchers, staleness detection

## 2. AST-based Analysis

### 2.1 Tree-sitter: The Universal Parser

Tree-sitter has become the de facto standard for editor-integrated code analysis:
- Fast, incremental, error-tolerant parsing with grammars for 100+ languages
- WASM-based runtime grammar loading (no recompilation needed)
- Concrete Syntax Trees (CSTs) with byte-precise positions
- S-expression query language for AST pattern matching

**Adoption across the ecosystem:**

| Tool | Languages | Mechanism | Key Innovation |
|------|-----------|-----------|----------------|
| grove | 27 | Tree-sitter WASM | symbol-id across turns, 7-tool MCP |
| code-graph-mcp | 16 | Tree-sitter + SQLite | Hybrid BM25+vector, RRF fusion |
| Codebase-Memory | 66 | Tree-sitter + SQLite | 2.1M-node binary, Louvain detection |
| doora | 7 | Tree-sitter + Bloom | Pre-parse rejection sieve |
| wonk | 12 | Tree-sitter + SQLite | 37% token reduction, ranked search |
| nervx | 7+ | Tree-sitter | Instance-method dispatch inference |
| Polyglot Indexer | 10+ | Tree-sitter + SCIP | Two-layer: AST + cross-file SCIP |

### 2.2 Key Tool Capabilities

**Grove** (Entelligentsia/grove, 2026) -- Most mature AST-only agent tool:
- 7 tools: outline, symbols, source, check, callers, map, definition
- Every result carries a symbol-id -- stable handle across turns
- Outline a 1700-line file as skeleton (~150 tokens); source one symbol (~50 tokens)
- Same Rust engine drives CLI and MCP server; grams load at runtime from WASM
- Delegated local-LLM mode: explore tool backed by Ollama

**doora** (backpack-lab/doora):
- S-expression queries for AST patterns
- Bloom filter trigram for pre-parse rejection
- Persistent SQLite structural index -- O(log n) symbol lookups
- Semantic rewriting: surgically replace AST nodes, MCP server

**wonk** (etr/wonk):
- Search ranking: definitions first, tests last, re-exports collapsed
- 37.4% total token reduction (per-task median 29.7%, best 68.5%)
- Quality maintained at 0.90 vs 0.91 baseline; 22 MCP tools

### 2.3 AST Pattern Matching vs. Regex

| Capability | grep/ripgrep | doora/ast-grep |
|------------|-------------|----------------|
| Find function definitions named "foo" | All occurrences | Only definition nodes |
| Functions taking exactly N arguments | Cannot express | Query parameter child count |
| Find calls outside test modules | Cannot scope | S-expression with predicate |
| Rename at every definition | Risks corrupting comments | AST-targeted, surgical |

### 2.4 ast-grep Structural Search

ast-grep provides YAML-based structural pattern matching for 25+ languages:
- Pattern matching by AST shape: console.log() matches any console.log call
- Meta-variables for flexible matching; Rules-as-YAML: composable codemods
- Use cases: migrating require() to import, stripping "as any"

## 3. Call Graph Construction

### 3.1 Static Call Graph Approaches

Modern static call graph construction for coding agents relies primarily on Tree-sitter extraction. Resolution strategies vary by language complexity:

**Simple resolution** (name-based):
- Match function calls to definitions by name
- Works well for Python, Ruby (dynamic but conventionally named)
- Used by: grove, wonk, doora

**Scope-aware resolution** (import-based):
- Trace imports to resolve qualified names (module.function() -> trace import)
- Used by: code-graph-mcp, codebase-memory, nervx

**Receiver-type inference** (instance-based):
- For OO languages: infer receiver type from local context
- Python: sp = SamplingParams() -> sp.verify() resolves to SamplingParams.verify
- Uses local assignments, annotations, parameter hints, self.foo = Foo() in __init__
- Implemented in: nervx (Python), code-graph-mcp (TypeScript/Rust/Java)

**LSP-hybrid resolution**:
- For C/C++/Go: augment Tree-sitter with LSP-style type resolution
- Used by: Codebase-Memory (Go, C, C++)

### 3.2 Dynamic Call Graph

Dynamic call graphs capture actual runtime dispatch:
- **nervx dispatches_to edges**: tracks which base-class methods dispatch to concrete overrides
  - confidence="low" flag for uncertain edges
- **OmniWeave**: models R's S4 setGeneric/setMethod as a dispatch graph
  - Routes generic calls through class -> method -> generic chain

### 3.3 Call Graph Tools Comparison

| Tool | Languages | Resolution | Query Modes | Scale |
|------|-----------|------------|-------------|-------|
| code-graph-mcp | 16 | Import + name + LSP-hybrid | Recursive CTE, cycle detection | 1M+ lines |
| nervx | 7+ | Import + receiver-type | BFS up to 6 hops, callers/callees | 50K files |
| wonk | 12 | Name + import | callers, callees, callpath, flows, blast | CI-scale |
| codebase-memory | 66 | Name + LSP-hybrid (Go/C/C++) | Inbound/outbound, config depth | 2.1M nodes |

### 3.4 Limitations

- Static call graphs hit a ceiling at runtime dispatch -- they route to declarations, not targets
- OmniWeave finding: on cross-process calls at scale (>1000 files), analysis ties with grep
- Hybrid combining static graphs with runtime traces shows promise but remains experimental

## 4. Vector Databases for Code

### 4.1 Code Embedding Models

**Commercial API Models:**

| Model | Dimensions | Cost/M tokens | Recall@10 | Notes |
|-------|-----------|:------------:|:---------:|-------|
| Voyage Code 3 | 256/512/1024 | .06 | 0.900 | Near-perfect MRR 0.973, 300+ langs |
| Voyage Code 2 | 1024 | .06 | 0.933 | 14.52% better than ada-002 |
| text-embedding-3-large | 256/1024/3072 | .13 | 0.900 | Best cost-performance balance |
| text-embedding-3-small | 512/1536 | .02 | 0.867 | 95% of voyage at 33% cost |
| codestral-embed | 1024 | -- | 0.900 | Best point estimate: 0.967 |

**Open-Source Models:**

| Model | Params | Dims | Optimal Recall | Notes |
|-------|--------|:----:|:--------------:|-------|
| CodeXEmbed 7B | 7B | 1024 | 0.90+ CoIR | SOTA open-source, Apache 2.0 |
| CodeXEmbed 2B | 2B | 1024 | 0.87+ CoIR | Beats Voyage-Code-2 by 20% |
| CodeXEmbed 400M | 400M | 1024 | 0.85+ CoIR | Best size-accuracy trade-off |
| qwen3-embed:0.6b | 600M | 1024 | 0.933 (c3000-o300) | Best local model |
| mxbai-embed-large | 335M | 1024 | 0.900 | Strong small model |
| Nomic Embed Code | 137M | 768 | 81.7% Python | Fully open-source |
| CodeT5+ | 110M | 256 | 0.764 | 86.7% gain with LoRA FT |
| MiniLM-L6 | 22M | 384 | 0.801 | 80% of voyage at  |

**Critical finding**: An embedding model cannot be evaluated independently from its retrieval pipeline. Each model has its own effective combination of chunking, retrieval mode, and query phrasing [Cognitive Benchmark, 2026].

### 4.2 Late Interaction Architectures

ColBERT-style late interaction outperforms single-vector dense retrieval on dependency and cross-file queries:

| Query Type | BM25 | GraphCodeBERT | UniXcoder | VoyageCode3 | **ColCode** |
|------------|:----:|:-------------:|:---------:|:-----------:|:-----------:|
| API usage | 0.42 | 0.58 | 0.63 | 0.67 | **0.69** |
| Dependency | 0.31 | 0.47 | 0.49 | 0.52 | **0.61** |
| Cross-file | 0.29 | 0.52 | 0.51 | 0.55 | **0.67** |
| **Average** | 0.38 | 0.55 | 0.57 | 0.61 | **0.68** |

MRR@10 on RepoSearch-1K [Neural Code Search, 2026]

Single-vector embeddings compress code into fixed-dimension vectors, losing token-level signals. Late interaction preserves token-level granularity -- critical for distinguishing "calls API X" from "imports module X".

### 4.3 Chunking Strategies

From systematic evaluation across 16 models, 5 chunk configs, 3 retrieval modes:

| Chunk Size | Overlap | Best For |
|-----------|---------|----------|
| c500-o100 | 100 | Small models (all-minilm) |
| **c1500-o200** | 200 | **Optimal for most models** |
| c3000-o300 | 300 | qwen3-embedding (0.933) |
| c5000-o500 | 500 | text-embedding-3-small |
| whole-file | -- | Only works with select models |

**Key**: Optimal chunk size is model-dependent -- never transfer choices between models without retesting.

### 4.4 Hybrid Search (BM25 + Vector)

From empirical study across 16 models [Cognitive Benchmark, 2026]:
- Adding BM25 to vector search helped some models, reduced results for others
- Natural questions: BM25 recall@10=0.600 (below any embedding)
- Keyword queries: BM25 recall@10=0.833-0.867 (competitive)
- Inaccurate terms: BM25 fell to 0.400 (semantic maintains 0.700+)

**File search vs. Hybrid search** (Onyx AI, 510K docs):
- Hybrid: faster, cheaper, better recall for narrow queries
- File search (glob/grep/read): completeness awareness, natural metadata filtering
- Recommended: combine both techniques

## 5. Dependency Analysis

### 5.1 Import Resolution Architecture

Module graph construction follows a parse-resolve-link pipeline:

Source files -> Parse imports -> Resolve paths -> Build graph -> Detect cycles

**Resolution strategies by ecosystem:**

| Ecosystem | Tool | Mechanism | Features |
|-----------|------|-----------|----------|
| JS/TS | dependency-cruiser | Acorn + enhanced-resolve | 2.5M weekly, circular/orphan/rule validation |
| Python | import-cruiser | ast module + import resolution | HTML/SVG/DOT export, CI validation |
| Python | knot-imports | ast + Tarjan's algorithm | Zero deps, Mermaid output |
| Rust | cargo (built-in) | SAT solver, feature unification | Cycle detection in resolver |
| Rust | cargo-ferris-wheel | Cargo metadata + Tarjan's | Workspace cycles, change impact (ripples) |
| Java | Maven Resolver 2.x | BFS collector | Dependency mediation, conflict resolution |
| Multi | dep-why | Lock file parsing | npm, cargo, pip, all-paths tracing |

### 5.2 Circular Dependency Detection

All major systems use Tarjan's Strongly Connected Components (SCC) algorithm:
- **Cargo built-in** (check_cycles): DFS-based, visits each node once, tracks path
- **dep-why**: Suggests break points -- package with fewest in-cycle deps
- **import-cruiser**: Architecture rule validation for CI (validate --strict)
- **dependency-cruiser**: 17 built-in rules (no-circular, no-orphans, not-to-unresolvable)

### 5.3 Build System Awareness

Deep build system understanding enables accurate dependency graphs:

| System | Awareness Level | Info Extracted |
|--------|----------------|----------------|
| Maven | POM parsing | Coordinates, transitive management, version conflict resolution |
| npm/pnpm | Lock file parsing | Transitive paths, hoisting, workspaces |
| Cargo | Manifest + lock | Feature flags, workspace members, version resolution |
| pip/poetry | Lock file parsing | Dependency tree, dev/prod separation |
| webpack | Module graph | Entry points, code splitting, loaders, tree shaking |

**Key insight**: Lock files (package-lock.json, Cargo.lock, poetry.lock) encode the resolved dependency graph -- parse them instead of re-resolving.

### 5.4 Dependency-aware Retrieval (DAR)

The Hydra framework [ACL 2026] demonstrates:
- Standard BM25/UniXCoder misses 40-60% of ground-truth dependencies
- DAR fine-tunes UniXCoder to classify dependency existence
- Combined with BM25 similarity for usage examples
- Result: +5% Pass@1 over strongest baselines

## 6. Cross-language Understanding

### 6.1 The Polyglot Challenge

Real-world repos are increasingly polyglot: Python orchestrates R scripts, JS calls Rust WASM, Java uses JNI. LSP tools are scoped to one language.

### 6.2 OmniWeave: Cross-Boundary Analysis

OmniWeave (SolvingLab/OmniWeave, 2026) -- most advanced polyglot analysis graph:

**Cross-language edges** follow shell-outs:
- Python: subprocess.run([...]), os.system(...)
- JS/TS: child_process.*, exec.Command
- Go: exec.Command
- Workflow: Snakemake rules, Nextflow processes

**Cross-process resolution** handles array and flat-string forms, f-string "this-directory" dispatcher pattern, __main__ entry points. Rejects unresolvable patterns.

**Workflow data-flow DAGs**: Snakemake/Nextflow pipelines become a graph with shared artifact nodes.

**Honest limitation**: OmniWeave does not make agents more correct -- capable agents tie it with grep. The moat is effort reduction: fewer tool calls, tokens, latency, and cost.

### 6.3 AXA: Cross-Language Static Analysis

AXA (TU Darmstadt) integrates existing single-language analyses:
- Language detectors for foreign function calls and memory accesses
- Translators between single-language analysis representations
- Coordinator for interleaved execution (global fixed-point)
- Validated on Java-JavaScript (ScriptEngine) and Java-Native (JNI)
- >98% reuse of existing analysis code

### 6.4 Polyglot Indexer: Two-Layer Architecture

Polyglot Indexer (davidroman0O, 2026):
- Layer 1 -- Tree-sitter: Parse every file, extract symbols, SQLite + FTS5
- Layer 2 -- SCIP: Compiler-accurate cross-file intelligence (definitions, references, implementations)
- SCIP indexers auto-discovered from standard install locations
- Cached with fingerprint-based change detection

### 6.5 Language-Agnostic Code Embeddings

Research [NAACL 2024] shows multilingual code embeddings have two components:
1. Language-specific (syntax): tied to language grammar and idioms
2. Language-agnostic (semantics): capturing program meaning

Removing language-specific components improves cross-language retrieval by up to +17 MRR points using centering, low-rank decomposition (LRD), or common-specific LRD.

## 7. Context Retrieval for Agents

### 7.1 The Navigation Paradox

CodeCompass [Paipuru, 2026] formalizes: larger context windows do NOT eliminate the need for structural navigation. They shift failure from retrieval capacity to navigational salience.

Three-group task taxonomy:

| Group | Type | Example | Best Strategy |
|-------|------|---------|---------------|
| G1 | Semantic (keyword-match) | "Find the auth handler" | BM25 (100% coverage) |
| G2 | Structural (nearby deps) | "Update the User model" | Combined (graph + semantic) |
| G3 | Hidden dependencies | "Add logger to BaseRepository" affects all callers | **Graph navigation required** |

Key results (270 trials with Claude Opus):
- G3: Graph navigation 99.4% ACS vs Vanilla 76.2% -- **23.2 point improvement**
- BM25 on G3: 78.2% -- confirms keyword retrieval cannot surface architecturally distant deps
- Navigation vs Retrieval is fundamental: retrieval asks "what's similar?"; navigation asks "what's structurally connected?"

### 7.2 LARGER: Lexically Anchored Graph Exploration

LARGER [Hu et al., 2026] formalizes Lexically Anchored Structural Localization:
1. Turn lexical matches into high-precision structural entry points
2. Expose confidence-filtered local neighborhoods within the agent's existing search loop
3. No external graph databases -- integrates directly into CLI coding agents

Results: +13.9 points on LocBench file-level Acc@5, consistent gains on MuLocBench and SWE-Atlas.

### 7.3 ContextBench: Measuring Retrieval Quality

ContextBench [Li et al., 2026] -- first systematic evaluation framework:
- 1,136 issue-resolution tasks from 66 repos across 8 languages
- 522,115 lines of human-verified gold contexts (23,116 classes/functions)
- Three granularity metrics: file, block (AST), and line levels
- Tracks context explored vs utilized vs discarded

Key findings:
- Sophisticated scaffolding yields only marginal gains in context retrieval -- "The Bitter Lesson"
- LLMs consistently favor recall over precision (explore more than necessary)
- Substantial gaps exist between explored and utilized context

### 7.4 DeepDiscovery: Task-Level Context Recovery

DeepDiscovery [2026] treats repository understanding as task-level context recovery:

Two-stage pipeline:
1. Location stage: Environment-aware analysis -> adaptive repo compression -> rule-guided anchor localization
2. Inference stage: Expand from anchors over multi-relational graph -> recover implementation path -> metadata-first context

Three relation sources:
- Explicit: imports, calls, inheritance, references
- Implicit: config-to-code, registration sites, DI wiring, event/callback bindings, test-to-implementation bridges
- Organizational: folder containment, module boundaries, ownership cues, physical proximity

### 7.5 What to Retrieve at Each Step

| Step | What to Retrieve | How | Token Budget |
|------|-----------------|-----|:------------:|
| 1. Query understanding | Entry-point candidates | Keyword + semantic + centrality | ~500 |
| 2. Anchor localization | High-confidence files/symbols | Graph-anchored lexical match | ~2K |
| 3. Neighborhood expansion | 1-2 hop structural context | Dep/call/inheritance graphs | ~5K |
| 4. Implicit link recovery | Registration, config, events | Rule library + lightweight LLM | ~3K |
| 5. Context assembly | Metadata + promoted text spans | Budget-aware gain/cost scoring | Remaining |

### 7.6 Testing Strategies for Retrieval Quality

Recommended evaluation framework (synthesized from ContextBench + vexp + cognitive benchmark):

**Metrics**:
- Recall@k, Precision, F1 (file/block/line level)
- Relevance Ratio: useful tokens / total tokens retrieved
- AUC-Cov: area under coverage curve across trajectory
- Veto Protocol: cases where internal search fails but graph traversal succeeds

**Testing dimensions**:
1. Query variability: natural, technical, keywords, inaccurate, cross-module
2. Task types: bug fix, feature addition, refactor, exploration, review
3. Repository scales: <10K, 10K-100K, 100K-1M, >1M LOC
4. Language diversity: single vs polyglot
5. Index freshness: up-to-date vs stale (measure sync drift impact)

## 8. Synthesis & Recommendations

### 8.1 Architecture Decision Framework

`
Query Characteristics:
  Natural language? -> Need semantic search
  Keyword/precise?  -> BM25 may suffice
  Cross-file?       -> Graph traversal essential

Codebase Characteristics:
  <50K lines    -> Embeddings work fine
  50K-500K      -> Graph-based preferred
  >500K         -> Graph essential for scale
  Polyglot      -> Cross-language graph needed

Primary Task Types:
  Mostly modification -> Graph-first approach
  Mostly exploration  -> Embedding + Graph hybrid
  Mixed              -> RAG with hybrid retrieval
`

### 8.2 Recommended Stack

**For maximum token efficiency (modification-heavy tasks):**
- Parser: Tree-sitter (27+ languages via WASM)
- Indexer: Dependency graph with symbol-level nodes
- Search: Keyword (BM25/FTS5) + graph traversal
- Vector optional: Late-interaction (ColBERT-style) for semantic fallback
- Storage: SQLite (single binary, zero deps)
- Protocol: MCP (Model Context Protocol)
- Expected: 0.65-0.85 relevance ratio, 65-70% token reduction

**For maximum flexibility (mixed tasks):**
- All of the above + dedicated embedding (Voyage Code 3 API or CodeXEmbed 400M local)
- Hybrid retrieval with RRF fusion
- Cost: +100-500ms latency, +.004-0.06/query

### 8.3 Open Challenges

1. Index freshness: Stale indexes cause "confident wrong" behavior -- real-time incremental indexing unsolved at scale
2. Cross-language call resolution: Runtime dispatch and subprocess commands remain hard ceilings
3. Evaluation standardization: No widely adopted retrieval quality benchmark
4. Token efficiency measurement: Most agents don't measure relevance ratio
5. The Navigation Paradox: Structural navigation (not context capacity) is the bottleneck

### 8.4 Emerging Research Directions

- Late-interaction architectures for code (ColCode pattern)
- Language-agnostic decomposition for cross-language transfer
- Multi-agent retrieval with specialized sub-agents
- Pre-indexed blueprint representations (AOCI)
- Budget-aware adaptive retrieval (FastCode/DeepDiscovery)

## 9. References

### Academic Papers

1. FastCode (2026). arXiv:2603.01012.
2. AOCI (2026). arXiv:2605.02421.
3. Codebase-Memory (2026). arXiv:2603.27277.
4. LARGER (2026). arXiv:2605.16352.
5. CodeCompass (2026). arXiv:2602.20048.
6. ContextBench (2026). arXiv:2602.05892.
7. DeepDiscovery (2026). arXiv:2606.22906.
8. Hydra (2026). ACL 2026.
9. AIRCoder (2026). ACL 2026.
10. Neural Code Search (2026). DM Journal.
11. Language Agnostic Code Embeddings (2024). NAACL 2024.
12. CodeXEmbed (2024). arXiv:2411.12644.
13. CodeBERT (2020). EMNLP 2020.
14. GraphCodeBERT (2021). ICLR 2021.
15. CodeT5+ (2023). EMNLP 2023.
16. ColBERT (2020). SIGIR 2020.
17. Do Not Treat Code as Natural Language (2026). arXiv:2602.11671.
18. TypeScript Repository Indexing (2026). arXiv:2604.18413.
19. A Cognitive Benchmark for Code-RAG (2026). DEV Community.

### Tools & Systems

20. grove -- Entelligentsia/grove. tree-sitter CLI + MCP, 27 languages.
21. code-graph-mcp -- sdsrss/code-graph-mcp. AST knowledge graph, 16 languages.
22. wonk -- etr/wonk. Structure-aware ranked search, 12 languages.
23. doora -- backpack-lab/doora. Structural code search with Bloom filter.
24. nervx -- adityakamat24/nervx. Code analysis with dispatch tracking.
25. OmniWeave -- SolvingLab/OmniWeave. Cross-language/cross-process graph.
26. Polyglot Indexer -- davidroman0O/polyglot-indexer-mcp. tree-sitter + SCIP.
27. code-graph-rag -- codehornets/code-graph-rag. Memgraph + tree-sitter + RAG.
28. vexp -- vexp.dev. Graph-first indexing, 30 languages.
29. dependency-cruiser -- sverweij/dependency-cruiser. JS/TS (2.5M weekly).
30. import-cruiser -- kevin91nl/import-cruiser. Python import analysis.
31. dep-why -- sudokatie/dep-why. Multi-ecosystem transitive tracing.
32. cargo-ferris-wheel -- Rust workspace cycle detection.
33. Aider -- paul-gauthier/aider. RepoMap graph-based indexing.

### Benchmarks

34. RepoSearch-1K -- 1000 queries across 5 languages.
35. ContextBench -- 1,136 tasks, 66 repos, 8 languages.
36. CoIR -- Code Information Retrieval benchmark.
37. CodeSearchNet -- 2M (NL, code) pairs, 6 languages.
38. DevEval / RepoExec -- Repository-level code generation.
39. LocBench / MuLocBench -- Code localization.
40. SWE-bench / SWE-bench Lite -- Software engineering task resolution.

---

*Report compiled July 2026. Sources include academic papers (arXiv, ACL, NAACL, EMNLP, ICLR, SIGIR), production tool documentation, open-source repository READMEs, and independent benchmarks.*
