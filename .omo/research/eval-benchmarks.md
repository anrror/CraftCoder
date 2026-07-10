# AI Coding Agent Evaluation: Comprehensive Benchmark & Framework Research Report

**Date**: July 2026  
**Author**: AI-OS Evaluation & Benchmarking Specialist  
**Version**: 1.0

---

## Table of Contents

1. [HumanEval](#1-humaneval)
2. [SWE-bench / SWE-bench Verified](#2-swe-bench--swe-bench-verified)
3. [Other Benchmarks](#3-other-benchmarks)
4. [Benchmark Infrastructure](#4-benchmark-infrastructure)
5. [Metrics Deep Dive](#5-metrics-deep-dive)
6. [Continuous Evaluation](#6-continuous-evaluation)
7. [Custom Benchmark Creation](#7-custom-benchmark-creation)
8. [Known Pitfalls](#8-known-pitfalls)
9. [References](#9-references)

---

## 1. HumanEval

### 1.1 Overview

HumanEval is the seminal code generation benchmark introduced by OpenAI in 2021 alongside the Codex paper ("Evaluating Large Language Models Trained on Code", Chen et al., 2021). It consists of **164 hand-written programming problems** designed to measure functional correctness of synthesized programs from docstrings.

**Key Properties:**
- **Size**: 164 problems
- **Language**: Python only
- **Task**: Generate a function body from a signature + docstring
- **Evaluation**: Functional correctness via unit tests (average ~7.7 test cases per problem)
- **Metric**: pass@k
- **License**: MIT

Reference: [GitHub](https://github.com/openai/human-eval) | [Paper](https://arxiv.org/abs/2107.03374)

### 1.2 Task Format

Each problem contains:
- A function signature with type annotations
- A docstring describing the expected behavior
- A reference solution (hidden during eval)
- Multiple unit tests (assertions)

Example (from the [dataset](https://github.com/openai/human-eval/blob/master/data/HumanEval.jsonl.gz)):
`python
{
    "task_id": "HumanEval/0",
    "prompt": "from typing import List\n\ndef has_close_elements(numbers: List[float], threshold: float) -> bool:\n    \"\"\" Check if in given list of numbers, are any two numbers closer to each other than\n    given threshold.\n    >>> has_close_elements([1.0, 2.0, 3.0], 0.5)\n    False\n    >>> has_close_elements([1.0, 2.8, 3.0, 4.0, 5.0, 2.0], 0.3)\n    True\n    \"\"\"\n",
    "entry_point": "has_close_elements",
    "canonical_solution": "    for i in range(len(numbers)):\n        for j in range(i + 1, len(numbers)):\n            if abs(numbers[i] - numbers[j]) < threshold:\n                return True\n    return False\n",
    "test": "def check(candidate):\n    assert candidate([1.0, 2.0, 3.0], 0.5) == False\n    assert candidate([1.0, 2.8, 3.0, 4.0, 5.0, 2.0], 0.3) == True\n    # ... more tests"
}
`

### 1.3 The pass@k Metric

The primary metric is **pass@k**, which estimates the probability that at least one out of k samples passes all unit tests.

**The unbiased estimator** (from the [official implementation](https://github.com/openai/human-eval/blob/master/human_eval/evaluation.py)):

`python
def estimate_pass_at_k(num_samples, num_correct, k):
    def estimator(n, c, k):
        """Calculates 1 - comb(n - c, k) / comb(n, k)."""
        if n - c < k:
            return 1.0
        return 1.0 - np.prod(1.0 - k / np.arange(n - c + 1, n + 1))

    return np.array([estimator(n, c, k) for n, c in zip(num_samples, num_correct)])
`

**Important**: This is an **unbiased estimator** (per problem, then averaged). If a problem has fewer samples than k, it cannot be estimated. Typical reporting values are pass@1, pass@10, and pass@100.

### 1.4 How to Implement a Runner

**Step 1: Setup**
`ash
git clone https://github.com/openai/human-eval
pip install -e human-eval
`

**Step 2: Generate completions** (saved as JSONL):
`python
from human_eval.data import write_jsonl, read_problems

problems = read_problems()
samples = [
    dict(task_id=task_id, completion=generate_one_completion(problems[task_id]["prompt"]))
    for task_id in problems
    for _ in range(num_samples_per_task)
]
write_jsonl("samples.jsonl", samples)
`

**Step 3: Evaluate**
`ash
evaluate_functional_correctness samples.jsonl
# Output: {'pass@1': ..., 'pass@10': ..., 'pass@100': ...}
`

**Security**: The [execution code](https://github.com/openai/human-eval/blob/master/human_eval/execution.py) is intentionally disabled by default. You MUST:
1. Read the security disclaimer
2. Uncomment the exec() call
3. Run in a sandboxed environment (Docker container)

**Using the HuggingFace evaluate library**:
`python
import evaluate
code_eval = evaluate.load("code_eval")
# Set HF_ALLOW_CODE_EVAL=1 first
pass_at_k, results = code_eval.compute(
    references=test_cases,
    predictions=candidates,
    k=[1, 10, 100],
    num_workers=16,
    timeout=3.0
)
`

### 1.5 Known Limitations

| Limitation | Details |
|---|---|
| **Data Contamination** | The 164 problems are publicly available and likely included in training data of modern models. Saturation is occurring — many models score >90% pass@1. |
| **Simplicity** | Tasks are single-function, self-contained, Python-only. They don't test real-world software engineering skills (multi-file, dependencies, debugging). |
| **Small Size** | 164 problems provide limited statistical power. Performance variance between runs can be significant. |
| **Python Only** | Does not measure multi-language capability. |
| **Docstring Ambiguity** | Some problems have ambiguous or incomplete specifications. |
| **No Execution Cost** | Fails to evaluate code quality, efficiency, or maintainability — only functional correctness. |

**Saturation Evidence**: By 2026, GPT-4o and Claude Opus 4 achieve >92% pass@1 on HumanEval, making it nearly useless for differentiating frontier models. As noted in the JetBrains Benchmark Meaning Gap blog (June 2026), "top models are within 2-3 points of each other on HumanEval, making it a coin flip for ranking."

---

## 2. SWE-bench / SWE-bench Verified

### 2.1 Overview

SWE-bench (Jimenez et al., 2024, ICLR 2024 Oral) is a benchmark for evaluating LLMs on **real-world software issues** collected from GitHub. Given a codebase and an issue description, the model must generate a patch that resolves the problem.

**Key Properties:**
- **Full set**: 2,294 task instances from 12 Python repositories
- **SWE-bench Lite**: 300 hand-picked instances (balanced, filtered)
- **SWE-bench Verified**: 500 human-validated instances (collaboration with OpenAI)
- **Evaluation**: Docker-based, deterministic test execution
- **Metric**: Resolve rate (binary: all designated tests pass)
- **Repositories**: Django, Flask, matplotlib, pylint, pytest, sympy, etc.

Reference: [GitHub](https://github.com/SWE-bench/SWE-bench) | [Leaderboard](https://www.swebench.com/)

### 2.2 Task Format

Each instance contains:
- epo: Repository name (e.g., sympy/sympy)
- instance_id: Unique identifier (e.g., sympy__sympy-20590)
- ase_commit: The repository state BEFORE the fix
- problem_statement: The GitHub issue description
- hints_text: Optional hints from issue comments
- patch: The gold (human-written) patch
- FAIL_TO_PASS: Tests that should FAIL before fix and PASS after
- PASS_TO_PASS: Tests that should remain PASSING after fix

**The key insight**: SWE-bench uses a **"FAIL_TO_PASS" and "PASS_TO_PASS"** test regime. Resolution requires:
1. All FAIL_TO_PASS tests must pass (the bug is fixed)
2. All PASS_TO_PASS tests must still pass (no regressions)

### 2.3 The SWE-bench Verified Subset

OpenAI and SWE-bench collaborators created a **human-validated** subset of 500 instances ([announcement](https://openai.com/index/introducing-swe-bench-verified/)):
- Human annotators verified each issue is solvable
- Tests were reviewed for correctness
- Reduced flaky test issues
- Became the de-facto standard for reporting

### 2.4 The SWE-bench Lite Subset

A minimal subset of 12 tasks specifically designed for quick evaluation rounds. Covers the 12 repositories: django, matplotlib, sympy, pytest, flask, pylint, sphinx, etc.

### 2.5 Evaluation Environment Setup

SWE-bench uses **Docker containers** for reproducible evaluation:

`ash
# Install
git clone https://github.com/SWE-bench/SWE-bench.git
cd SWE-bench
pip install -e .

# Run evaluation
python -m swebench.harness.run_evaluation \
    --dataset_name princeton-nlp/SWE-bench_Verified \
    --predictions_path /path/to/predictions.jsonl \
    --max_workers 8 \
    --run_id my_eval

# Or using sb-cli (cloud-based, free)
sb-cli submit swe-bench_verified test \
    --predictions_path preds.json \
    --run_id my-run
`

**Resource requirements** (from the [docs](https://github.com/SWE-bench/SWE-bench)):
- x86_64 machine with 120GB+ free storage, 16GB+ RAM, 8+ CPU cores
- Docker Desktop needs virtual disk space set to ~120GB
- ARM64 support is experimental
- --max_workers ≤ min(0.75 * os.cpu_count(), 24)

### 2.6 Performance Results (2024-2026)

| Model / Agent | Score | Date | Notes |
|---|---|---|---|
| Claude 2 (oracle retrieval) | 4.8% | Oct 2023 | Baseline |
| Devin (Cognition) | 13.86% | Mar 2024 | First commercial agent |
| Amazon Q Developer | 20.3% (Lite) | May 2024 | |
| Claude 3.5 Sonnet | 33% | Oct 2024 | |
| Claude 3.5 Sonnet (new) | 49% | Nov 2024 | Major jump |
| Gemini 3 Flash + mini-SWE-agent v2 | 75.8% | Feb 2026 | |
| Claude 4.5 Opus + mini-SWE-agent v2 | 76.8% | Feb 2026 | Top of bash-only leaderboard |
| Claude Opus 4.8 | 69.2% (SWE-bench Pro) | May 2026 | Harder variant |

**Key insight**: Score improvements come from BOTH model improvements AND scaffolding improvements. Mini-SWE-agent v2's switch from parsing bash output to tool calling alone contributed several percentage points.

### 2.7 Known Issues

| Issue | Details |
|---|---|
| **Flaky tests** | Some tests are non-deterministic. Verified subset reduced this. |
| **Dependency hell** | Docker images are large (2-8GB each). Full eval needs >100GB bandwidth. |
| **Docker availability** | Requires Docker; ARM64 support experimental. Singularity alternative for HPC. |
| **Score variance** | Small samples (10 instances) produce unreliable results. |
| **Contamination** | OpenAI stopped reporting SWE-bench Verified (Feb 2026). >32% of "passed" cases leaked solutions per [arXiv:2410.06992](https://arxiv.org/abs/2410.06992). |
| **Python-only** | Limited to Python, not representative of multi-language dev. |
| **Bug-fix bias** | All tasks are bug fixes, not feature development or refactoring. |

### 2.8 SWE-agent and Devin

**SWE-agent** ([GitHub](https://github.com/SWE-agent/SWE-agent)): Open-source agent framework designed for SWE-bench. Uses specialized agent-computer interface (ACI). Mini-SWE-agent v2 achieves >74% on SWE-bench Verified with ~100 lines of core logic.

**Devin** (Cognition): First commercial AI software engineer. Scored 13.86% (Mar 2024). Their later SWE-1.7 (Jul 2026) claims frontier intelligence but no longer reports SWE-bench scores.

**ForgeJudge** ([GitHub](https://github.com/ahmedEid1/forgejudge)): Open-source evaluation harness with CI gate, mutation hardening, cheat-resistant grading. Achieves 90.7% pass@1 on custom golden set (gpt-oss-120b).

---

## 3. Other Benchmarks

### 3.1 MBPP (Mostly Basic Programming Problems)

- **Source**: Google Research (Austin et al., 2021)
- **Size**: 974 problems (full), 427 (sanitized)
- **Language**: Python
- **Task**: Generate function from NL description + test cases
- **Metric**: pass@k (3 test cases per problem)
- **Splits**: Test (IDs 11-510), few-shot (1-10), validation (511-600), train (601-974)
- **Available**: [HuggingFace](https://huggingface.co/datasets/google-research-datasets/mbpp)

**Known issues**: Minimal tests (3/problem), higher false positive risk. Trivial for modern models. The "sanitized" split has cleaner descriptions.

### 3.2 APPS (Automated Programming Progress Standard)

- **Source**: Hendrycks et al., NeurIPS 2021
- **Size**: 10,000 problems (5K train / 5K test)
- **Language**: Python
- **Difficulty**: Introductory (3,639), Interview (5,000), Competition (1,361)
- **Test cases**: 131,836 total (~21.2/problem)
- **Metrics**: Test case average, strict accuracy
- **Available**: [GitHub](https://github.com/hendrycks/apps)

**Key findings**: BLEU anticorrelated with accuracy. Syntax errors decrease exponentially. GPT-Neo passed ~20% test cases on intro problems. Modern models (2026): ~60-70% intro, ~30-40% interview, ~10-15% competition.

### 3.3 CodeContests

- **Source**: Google DeepMind (AlphaCode), 2022
- **Size**: ~13,000 problems
- **Sources**: Codeforces, Aizu, AtCoder, CodeChef, HackerEarth
- **Languages**: C++, Python, Java
- **Metric**: n@k (problems solved with n submissions from k samples)
- **Available**: [GitHub](https://github.com/google-deepmind/code_contests)

**AlphaCode Results**: Top 54.3% in Codeforces competitions (generating millions of programs, filtering to 10 submissions). **CodeContests+** (2025): improved test case quality.

### 3.4 CodeXGLUE

- **Source**: Microsoft Research, 2021 ([paper](https://arxiv.org/abs/2102.04664))
- **Size**: 14 datasets across 10 tasks
- **Languages**: Multiple (Python, Java, etc.)
- **Categories**: Code-Code (clone, defect, completion, repair, translation), Text-Code (search, generation), Code-Text (summarization), Text-Text (doc translation)
- **Available**: [GitHub](https://github.com/microsoft/CodeXGLUE) | [Website](https://microsoft.github.io/CodeXGLUE/)

**Key tasks**: Code completion (line-level, 10K Python + 365 Java, EM + Edit Similarity), Code search (AdvTest, 19K queries, MRR), Code repair.

### 3.5 BigCodeBench

- **Source**: BigCode Project, ICLR 2025 ([paper](https://arxiv.org/abs/2406.15877))
- **Size**: 1,140 tasks (Hard subset: 148)
- **Language**: Python
- **Splits**: Complete (code completion), Instruct (NL->code)
- **Avg test cases**: 5.6 with **99% branch coverage**
- **Libraries**: 77 standard + 62 third-party
- **Available**: [GitHub](https://github.com/bigcode-project/BigCodeBench) | [Leaderboard](https://bigcode-bench.github.io/)

**Why it matters**: Addresses HumanEval simplicity — requires 4.7 function calls/task across 139 libraries, 577 unique library combos, complex compositional reasoning.

**Usage**:
`ash
pip install bigcodebench
bigcodebench.generate --model meta-llama/Meta-Llama-3.1-8B-Instruct --split instruct
bigcodebench.evaluate --model meta-llama/Meta-Llama-3.1-8B-Instruct --split instruct --execution local
`

### 3.6 CRUXEval (Code Reasoning, Understanding, and eXecution Evaluation)

- **Source**: Facebook Research (Gu et al., 2024) - [paper](https://arxiv.org/abs/2401.03065)
- **Size**: 800 Python functions (3-13 lines)
- **Tasks**: CRUXEval-I (input prediction), CRUXEval-O (output prediction)
- **Metric**: pass@1
- **Available**: [GitHub](https://github.com/facebookresearch/cruxeval) | [Website](https://crux-eval.github.io/)

**Key findings**: GPT-4 with CoT: 75%/81% on input/output prediction. Code Llama 34B: 50%/46%. Many HumanEval high-scorers show NO improvement on CRUXEval. Tests execution reasoning, not generation.

### 3.7 REPOCOD

- **Source**: Liang et al., ACL 2025 ([paper](https://aclanthology.org/2025.acl-long.1204/))
- **Size**: 980 whole-function tasks
- **Repos**: 11 popular Python projects (avg 2,610 files, 290K LoC)
- **Context**: 50.8% require repository-level context
- **Avg tests/task**: 313.5 (developer-written)
- **Avg solution**: 331.6 tokens, cyclomatic complexity 9.0
- **Available**: [GitHub](https://github.com/lt-asset/REPOCOD)

**Results**: Best model (GPT-4o) 27.35% pass@1. No model >30%. Contrasts with ~90% on HumanEval — "SOTA LLMs are still far away from writing real-world programs."

### 3.8 CrossCodeEval

- **Source**: Amazon, NeurIPS 2023 ([paper](https://arxiv.org/abs/2310.11248))
- **Size**: 10,000 examples from 1,000 repos
- **Languages**: Python, Java, TypeScript, C#
- **Task**: Cross-file code completion (single-line)
- **Metrics**: Exact Match, Edit Similarity
- **Available**: [GitHub](https://github.com/amazon-science/cceval)

**Key finding**: Without cross-file context, best models <9% exact match. Adding context improves 3-4.5x. Shows single-file benchmarks are insufficient.

### 3.9 Other Notable Benchmarks

| Benchmark | Year | Focus | Size | Metric |
|---|---|---|---|---|
| DS-1000 | 2022 | Data science (NumPy, Pandas) | 1,000 | pass@1 |
| HumanEval+ / MBPP+ | 2023 | Enhanced tests (80x more) | 164/974 | pass@k |
| ClassEval | 2023 | OOP (classes) | 100 classes | pass@k |
| RepoBench | 2023 | Repository-level completion | 11 repos | Edit sim |
| LongCodeArena | 2024 | Long-context generation | 50 tasks | pass@1 |
| DevEval | 2024 | Developer tasks (14 repos) | 1,860 | pass@k |
| EvoEval | 2024 | Evolved (mutated) challenges | 1,000+ | pass@k |
| LiveCodeBench | 2024 | Recent comp. problems (time-locked) | 600+ | pass@1 |
| SWE-bench Multimodal | 2025 | Visual software domains | 500 | Resolve rate |
| SWE-bench Multilingual | 2025 | Java, TypeScript, etc. | 2,200 | Resolve rate |
| Terminal-Bench 2.0 | 2026 | Terminal agent tasks | 89 | Task success |
| SWE-CI | 2026 | CI over time / maintainability | 100 | EvoScore |
| EffiBench-X | 2025 | Runtime + memory efficiency | multi | Efficiency |

---

## 4. Benchmark Infrastructure

### 4.1 Building a Scalable Evaluation Harness

A robust evaluation harness needs four core components:
1. **Task Runner**: Loads dataset, manages execution lifecycle
2. **Sandbox**: Isolated execution environment (Docker)
3. **Verifier**: Grading/evaluation logic
4. **Results Store**: Persistent storage + reporting

### 4.2 Architecture Template

`
                    ┌──────────────────────────────────────────┐
                    │        Orchestrator (eval loop)           │
                    │                                          │
                    │  for problem in dataset:                  │
                    │    sandbox = pool.acquire()               │
                    │    agent.solve(task, sandbox)             │
                    │    result = verifier.grade(sandbox)       │
                    │    store.save(result)                     │
                    │    pool.release(sandbox)                  │
                    └───────────┬──────────────┬────────────────┘
                                │              │
                    ┌───────────▼──┐    ┌──────▼──────────────┐
                    │ Model API   │    │ Docker Sandbox Pool  │
                    │ (vLLM/API)  │    │ image: per-task      │
                    │             │◄───┤ bridge network       │
                    │ :8000       │    │ ┌───────────────────┐│
                    └──────────────┘    │ │ Agent or code    ││
                                        │ └───────────────────┘│
                                        └──────────────────────┘
`

This architecture is used by: **Harbor** ([docs](https://harbor-framework-harbor.mintlify.app/)), **NVIDIA NeMo Evaluator** ([docs](https://docs.nvidia.com/nemo/evaluator/)), **ScaleBox** ([GitHub](https://github.com/icip-cas/ScaleBox)), **SanityHarness** ([GitHub](https://github.com/lemon07r/SanityHarness))

### 4.3 Docker-based Sandboxing

**Critical patterns**:

`python
# Pattern 1: Container-per-task (SWE-bench style)
for task in dataset:
    container = docker_client.containers.run(
        image=f"swebench/sweb.eval.{task.instance_id}:latest",
        command="sleep infinity",  # Keep alive
        detach=True, network="bridge",
        mem_limit="4g", cpu_period=100000, cpu_quota=800000,
    )
    container.exec_run("git apply /tmp/patch.diff")
    result = container.exec_run("pytest tests/ -x", timeout=300)
    container.stop(); container.remove()

# Pattern 2: Persistent container pool (faster)
pool = ContainerPool(prewarm=8)
for task in dataset:
    container = pool.acquire(image=get_image(task))
    container.upload(task.patch, "/tmp/patch.diff")
    result = container.exec("pytest tests/")
    pool.release(container)  # Container stays warm
`

**Key considerations**:
- **Image caching**: Pre-pull before evaluation
- **Network isolation**: --network none for code execution, bridge for agent tasks
- **Resource limits**: CPU quotas, memory limits, disk quotas
- **Cleanup**: Signal handlers + atexit for leaked containers
- **Timeouts**: Per-step + total evaluation timeouts

### 4.4 Result Caching

**Why**: Code gen evaluations are expensive (-500+ per full run).

**Layered caching approach**:
1. **Prompt-level**: Same prompt → same completion (deterministic models)
2. **Completion-level**: Same completion → same test results
3. **Task-level**: Task already evaluated for model → skip
4. **Container-level**: Docker image layer caching

`python
class EvalCache:
    def get(self, task_id, model, params):
        key = hash(task_id + model + str(params))
        return self._cache.get(key)
    def set(self, task_id, model, params, result):
        key = hash(task_id + model + str(params))
        self._cache.set(key, result, ttl=3600*24*7)
`

### 4.5 Parallel Execution

**Concurrency models**:

`python
# Thread-based (I/O bound - API calls)
with ThreadPoolExecutor(max_workers=16) as pool:
    futures = [pool.submit(agent.solve, task) for task in tasks]

# Process-based (CPU bound - Docker execution)
with Pool(processes=8) as pool:
    results = pool.map(evaluate_task, tasks)

# Async (high concurrency)
async def evaluate_all():
    sem = asyncio.Semaphore(16)
    async def bound_eval(task):
        async with sem:
            return await evaluate_one(task)
    return await asyncio.gather(*[bound_eval(t) for t in dataset])
`

**Resource management**: --max_workers ≤ min(0.75 * cpu_count(), 24). Each SWE-bench container uses 2-8GB. Reserve 120GB+ disk for full SWE-bench.

### 4.6 Cost Tracking per Evaluation Run

`python
class CostTracker:
    def log_api_call(self, model, prompt_tokens, completion_tokens, cost):
        ...
    def log_compute(self, duration_sec, instance_type, estimated_cost):
        ...
    def summary(self):
        return {"total_cost": ..., "api_cost": ..., "compute_cost": ...}
`

**Cost factors**: API (.15-15/M tokens), compute (Docker builds + execution), storage (images), CI/CD minutes.

**Typical costs (2026)**:
- HumanEval (164 tasks, 200 samples, GPT-4o): ~-150
- SWE-bench Verified (500 tasks, Claude Opus 4): ~-800
- REPOCOD (980 tasks, GPT-4o): ~-2000

---

## 5. Metrics Deep Dive

### 5.1 pass@k

**Definition**: Probability that at least one of k samples passes all unit tests.

**Formula**: pass@k = 1 - C(n-c, k) / C(n, k) where n = total samples, c = correct samples.

**Unbiased estimator** (OpenAI):
`python
def pass_at_k(n, c, k):
    if n - c < k: return 1.0
    return 1.0 - np.prod(1.0 - k / np.arange(n - c + 1, n + 1))
`

**Usage**: Standard for HumanEval, MBPP, BigCodeBench, REPOCOD. Reported as pass@1, pass@10, pass@100. For agents: pass@1 is primary (one attempt). For LMs: pass@100 meaningful (sample diversity).

### 5.2 Resolve Rate

**Definition**: Binary metric — did agent's patch pass ALL designated tests?

**Used by**: SWE-bench family.

`
Resolved iff:
  - All FAIL_TO_PASS tests pass (bug fixed)
  - All PASS_TO_PASS tests pass (no regressions)
  - Patch applies cleanly
`

**Sub-results**: esolved, patch_applied, F2P_success, P2P_success, egression, incorrect_fix.

### 5.3 Edit Similarity

edit_similarity = 1 - edit_distance(pred, ref) / max(len(pred), len(ref))

Used by CodeXGLUE, CrossCodeEval. Continuous scoring without execution. But doesn't measure semantic correctness.

### 5.4 Exact Match (EM)

Strict string equality. Used by CodeXGLUE, CrossCodeEval. Too strict for code (variable name difference = zero).

### 5.5 BLEU / CodeBLEU

**BLEU**: N-gram precision. **Discouraged** for code evaluation (APPS paper found it "sometimes anticorrelated with accuracy").

**CodeBLEU**: Enhanced with AST match + data flow match. Supplementary metric for translation/summarization only.

### 5.6 Functional Correctness (Test Case Pass Rate)

Percentage of test cases passing. Used by APPS (partial credit), HumanEval+. More granular than binary pass/fail.

### 5.7 What Matters for Coding Agents vs Language Models

| Aspect | Language Models | Coding Agents |
|---|---|---|
| Primary metric | pass@k | Resolve rate |
| Key capability | Syntax, algorithms | Multi-step, debugging, tools |
| Sample budget | 100+ samples | Usually 1 attempt |
| Environment | Static, single-function | Dynamic, multi-file |
| Verification | Unit tests | Full suite + regression check |
| Cost consideration | Token generation | API calls + runtime |
| Important sub-metrics | Syntax error rate | Patch success, regression rate, time-to-fix |

**Additional agent metrics needed**:
- **Cost per resolved instance**: $/resolved
- **Time to resolve**: Wall-clock time
- **Regression rate**: How often existing functionality breaks
- **Patch quality**: Lines changed, complexity
- **EvoScore** (SWE-CI): Correctness on future modifications

---

## 6. Continuous Evaluation

### 6.1 CI/CD Integration

**Why**: Model upgrades cause regressions, prompt changes affect behavior, scaffolding changes interact unpredictably.

`yaml
name: Continuous Evaluation
on:
  schedule: [{cron: '0 6 * * 1'}]
  push: {branches: [main], paths: ['agent/**', 'prompts/**']}
jobs:
  evaluate:
    steps:
      - run: python -m eval.harness --benchmarks humaneval,basic_swedev
      - run: python -m eval.regression_check --baseline baseline.json --current current.json
`

### 6.2 Regression Detection

**Statistical approach** (from ForgeJudge):

`python
def detect_regression(baseline, candidate, alpha=0.05):
    t_stat, p_value = stats.ttest_ind(baseline, candidate, equal_var=False)
    return t_stat < 0 and p_value / 2 < alpha
`

**Multi-seed**: Run with 3+ seeds (	emperature=0 not deterministic per [arXiv:2602.07150](https://arxiv.org/pdf/2602.07150)). Fail if candidate CI upper bound < baseline CI lower bound.

### 6.3 Leaderboard Design

**Essential fields**: model, agent, timestamp, per-benchmark scores, cost breakdown, metadata (git SHA, config, runtime).

**Categorization**: Bash-only (raw model), Full-system (commercial agents), Cost-aware, Time-to-solve.

### 6.4 A/B Testing Framework

Run baseline + candidate with N seeds each. Compute per-benchmark deltas and statistical significance. Flag regressions and improvements.

---

## 7. Custom Benchmark Creation

### 7.1 Template for a Task Instance

`python
@dataclass
class BenchmarkTask:
    task_id: str
    repository: str
    base_commit: str
    problem_statement: str
    context_files: list[str]
    test_oracle: str  # How to verify
    expected_patch: Optional[str]
    difficulty: str  # Introductory / Interview / Competition
    tags: list[str]
`

### 7.2 Automated Task Extraction (from REPOCOD pipeline)

1. **Repository Selection**: Popularity, language, test coverage, active maintenance
2. **Target Function Selection**: Extract functions, filter by complexity/length, classify context
3. **Test Case Collection**: Find developer-written tests, optimize to minimal coverage set (REPOCOD: 17,974→313)

### 7.3 Automated Verification Levels

- **Level 1**: Unit tests (pass/fail)
- **Level 2**: Property-based testing (hypothesis-style)
- **Level 3**: Differential testing (compare to reference on random inputs)

**Mutation hardening** (from ForgeJudge): Generate broken variants and verify tests catch them.

### 7.4 Quality Assurance Checklist

- [ ] **No data leakage**: Check overlap with The Stack, GitHub commits
- [ ] **Solvable**: Human experts can consistently solve
- [ ] **Deterministic tests**: Same result every run
- [ ] **Minimal flakiness**: No network, random, or timing-sensitive tests
- [ ] **Regression coverage**: Tests catch failures AND regressions
- [ ] **Sufficient test cases**: Avoid false positives
- [ ] **Difficulty calibration**: Distribution matches intended use
- [ ] **Reproducible**: Docker/environment pinned to specific versions

---

## 8. Known Pitfalls

### 8.1 Data Contamination

**The problem**: Training data includes benchmark problems.

**Evidence**: OpenAI stopped reporting SWE-bench Verified (Feb 2026). 32% of "resolved" cases leaked solutions ([arXiv:2410.06992](https://arxiv.org/abs/2410.06992)). 31% of "passed" had weak tests.

**Mitigations**:
1. **Time-locked**: Use problems published after model cutoff (LiveCodeBench)
2. **Canary strings**: Unique identifiers in benchmarks
3. **Versioned datasets**: Regular task rotation
4. **Mutation hardening**: Ensure tests catch wrong solutions
5. **N-gram overlap analysis**: Report training data overlap
6. **Hidden tests**: Keep test cases private until evaluation

`python
def check_contamination(model_output, tasks):
    for task in tasks:
        similarity = SequenceMatcher(None, task.canonical_solution, model_output).ratio()
        if similarity > 0.9:
            return {"task_id": task.id, "contaminated": True, "similarity": similarity}
    return {"contaminated": False}
`

### 8.2 Overfitting to Benchmarks

**Symptoms**: High benchmark score but poor real-world performance. Widening gap over time.

**Countermeasures**: Multi-benchmark evaluation, rotating benchmarks, real-world validation correlations, blind evaluation.

### 8.3 Metric Gaming

| Strategy | Benchmark | Mitigation |
|---|---|---|
| Sample inflation | HumanEval (pass@100) | Report pass@1 alongside pass@k |
| Skipping tests | SWE-bench | Cheat-resistant grading (restore oracle tests) |
| Verbose output | Edit similarity | Normalize whitespace, comments |
| Template overfitting | Code completion | Vary prompt templates |

**ForgeJudge cheat-resistant grader**: sandbox.exec("git checkout -- tests/") before grading to prevent patch from neutering tests.

### 8.4 Environment Reproducibility

**Common failures**: Docker image rot, network dependencies, hardware variance, randomness, time-dependent behavior.

**Best practices**: Pin ALL versions (use SHA digests), use lock files, avoid network dependencies in tests.

### 8.5 What Benchmarks Miss

| Capability | Not measured |
|---|---|
| Code quality | Readability, maintainability, best practices |
| Security | Vulnerability introduction |
| Performance | Runtime efficiency, memory usage |
| Testing | Did model write tests for changes? |
| Documentation | Did model update docs? |
| Collaboration | Code review, PR description |
| Long-term maintenance | Can code be maintained over time? |
| Debugging | Interactive bug finding |
| Multi-language | Working across language boundaries |

**Emerging benchmarks addressing gaps**: EffiBench-X (efficiency), SWE-CI (maintainability), Multi-Docker-Eval (environments), Terminal-Bench 2.0 (terminal tasks).

---

## 9. References

### 9.1 Papers

| Paper | Venue | Year |
|---|---|---|
| [Evaluating LLMs Trained on Code (HumanEval)](https://arxiv.org/abs/2107.03374) | arXiv | 2021 |
| [Program Synthesis with LLMs (MBPP)](https://arxiv.org/abs/2108.07732) | arXiv | 2021 |
| [Measuring Coding Challenge Competence (APPS)](https://arxiv.org/abs/2105.09938) | NeurIPS | 2021 |
| [CodeXGLUE](https://arxiv.org/abs/2102.04664) | NeurIPS | 2021 |
| [AlphaCode (CodeContests)](https://arxiv.org/abs/2203.07814) | Science | 2022 |
| [SWE-bench](https://arxiv.org/abs/2310.06770) | ICLR Oral | 2024 |
| [CRUXEval](https://arxiv.org/abs/2401.03065) | ICML | 2024 |
| [BigCodeBench](https://arxiv.org/abs/2406.15877) | ICLR | 2025 |
| [CrossCodeEval](https://arxiv.org/abs/2310.11248) | NeurIPS | 2023 |
| [REPOCOD](https://aclanthology.org/2025.acl-long.1204/) | ACL | 2025 |
| [SWE-CI](https://www.arxiv.org/pdf/2603.03823) | arXiv | 2026 |
| [Data Contamination](https://arxiv.org/abs/2410.06992) | arXiv | 2024 |

### 9.2 Repositories & Tools

| Resource | URL |
|---|---|
| HumanEval | https://github.com/openai/human-eval |
| SWE-bench | https://github.com/SWE-bench/SWE-bench |
| BigCodeBench | https://github.com/bigcode-project/BigCodeBench |
| CRUXEval | https://github.com/facebookresearch/cruxeval |
| CodeXGLUE | https://github.com/microsoft/CodeXGLUE |
| APPS | https://github.com/hendrycks/apps |
| CodeContests | https://github.com/google-deepmind/code_contests |
| CrossCodeEval | https://github.com/amazon-science/cceval |
| REPOCOD | https://github.com/lt-asset/REPOCOD |
| SWE-agent | https://github.com/SWE-agent/SWE-agent |
| mini-SWE-agent | https://github.com/SWE-agent/mini-swe-agent |
| Harbour | https://harbor-framework-harbor.mintlify.app/ |
| ScaleBox | https://github.com/icip-cas/ScaleBox |
| SanityHarness | https://github.com/lemon07r/SanityHarness |
| ForgeJudge | https://github.com/ahmedeid1/forgejudge |
| EffiBench-X | https://github.com/EffiBench/EffiBench-X |
| sb-cli | https://github.com/swe-bench/sb-cli |
| LM Eval Harness | https://github.com/EleutherAI/lm-evaluation-harness |
| HuggingFace Evaluate | https://github.com/huggingface/evaluate |
| NeMo Evaluator | https://docs.nvidia.com/nemo/evaluator/ |

---

*Generated July 2026. This report should be updated quarterly as benchmarks evolve rapidly.*
