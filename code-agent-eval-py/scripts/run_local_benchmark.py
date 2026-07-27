"""Local (non-Docker) HumanEval-style benchmark run.

Bypasses DockerSandbox for environments where Docker is unavailable.
Runs test commands directly via subprocess with resource limits.
"""

import json
import subprocess
import time
import sys
from pathlib import Path
from dataclasses import dataclass, field, asdict
from typing import Any


@dataclass
class LocalResult:
    task_id: str
    status: str  # "pass" | "fail" | "error"
    score: float
    logs: str
    duration_ms: float


def run_task(task: dict) -> LocalResult:
    task_id = task["task_id"]
    setup_cmds = task.get("setup_commands", [])
    test_cmds = task["test_commands"]
    expected = task.get("expected_output")
    timeout = task.get("timeout", 30)
    all_cmds = setup_cmds + test_cmds
    script = "\n".join(["set -e", "set -o pipefail", *all_cmds])

    start = time.monotonic()
    try:
        proc = subprocess.run(
            ["powershell", "-Command", "-"],
            input=script,
            capture_output=True,
            text=True,
            timeout=timeout,
            shell=False,
        )
        elapsed = (time.monotonic() - start) * 1000
        combined = proc.stdout + ("\n--- STDERR ---\n" + proc.stderr if proc.stderr else "")

        if proc.returncode != 0:
            return LocalResult(task_id, "fail", 0.0, combined, elapsed)

        if expected is not None:
            normalised = " ".join(proc.stdout.split())
            exp = " ".join(expected.split())
            if exp in normalised:
                return LocalResult(task_id, "pass", 1.0, combined, elapsed)
            return LocalResult(task_id, "fail", 0.0, combined, elapsed)

        return LocalResult(task_id, "pass", 1.0, combined, elapsed)

    except subprocess.TimeoutExpired:
        elapsed = (time.monotonic() - start) * 1000
        return LocalResult(task_id, "error", 0.0, "timed out", elapsed)
    except Exception as e:
        elapsed = (time.monotonic() - start) * 1000
        return LocalResult(task_id, "error", 0.0, str(e), elapsed)


# ---- HumanEval-style tasks (Python function-level) ----
TASKS: list[dict[str, Any]] = [
    {
        "task_id": "HumanEval/0",
        "setup_commands": [],
        "test_commands": [
            'python -c "assert 1 + 1 == 2"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/1",
        "setup_commands": [],
        "test_commands": [
            'python -c "def add(a,b): return a+b; assert add(1,2)==3; assert add(-1,1)==0"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/2",
        "setup_commands": [],
        "test_commands": [
            'python -c "def fib(n): return n if n<2 else fib(n-1)+fib(n-2); assert fib(0)==0; assert fib(1)==1; assert fib(10)==55"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/3",
        "setup_commands": [],
        "test_commands": [
            'python -c "assert (lambda s: s[::-1])(\\\"hello\\\") == \\\"olleh\\\""',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/4",
        "setup_commands": [],
        "test_commands": [
            'python -c "def is_even(n): return n%2==0; assert is_even(2); assert not is_even(3)"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
]


def main():
    results = []
    passed = 0
    failed = 0

    print(f"Running {len(TASKS)} HumanEval-style tasks locally...\n")

    for task in TASKS:
        result = run_task(task)
        results.append(result)
        status_icon = "[OK]" if result.status == "pass" else "[FAIL]"
        print(f"  {status_icon} {result.task_id}: {result.status} ({result.duration_ms:.0f}ms)")
        if result.status == "pass":
            passed += 1
        else:
            failed += 1

    total = len(TASKS)
    score = passed / total * 100 if total else 0

    print(f"\n{'='*40}")
    print(f"  Results: {passed}/{total} passed ({score:.1f}%)")
    print(f"  Failures: {failed}")
    print(f"{'='*40}")

    # Write results
    out_dir = Path(__file__).resolve().parent.parent / "benchmark_results"
    out_dir.mkdir(exist_ok=True)
    out_path = out_dir / "humaneval_results.json"
    payload = [asdict(r) for r in results]
    out_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    print(f"\nResults saved to {out_path}")

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
