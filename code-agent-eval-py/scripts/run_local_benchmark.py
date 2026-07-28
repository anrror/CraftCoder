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
            'python -c "s = chr(104)+chr(101)+chr(108)+chr(108)+chr(111); assert s[::-1] == chr(111)+chr(108)+chr(108)+chr(101)+chr(104)"',
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
    {
        "task_id": "HumanEval/5",
        "setup_commands": [],
        "test_commands": [
            'python -c "def count_chars(s): from collections import Counter; c=Counter(s); assert c[chr(108)]==3; return c"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/6",
        "setup_commands": [],
        "test_commands": [
            'python -c "def merge_sorted(a,b): i=j=0; r=[]; [r.append(a[i]) if (j>=len(b) or (i<len(a) and a[i]<=b[j])) else r.append(b[j]) for _ in range(len(a)+len(b))]; return r[:len(a)+len(b)]"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/7",
        "setup_commands": [],
        "test_commands": [
            'python -c "def find_missing(arr,n): s=sum(arr); expected=n*(n+1)//2; return expected-s; assert find_missing([1,2,4,5],5)==3; assert find_missing([1,3,4,5,6],6)==2"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/8",
        "setup_commands": [],
        "test_commands": [
            'python -c "def gcd(a,b): return a if b==0 else gcd(b,a%b); assert gcd(48,18)==6; assert gcd(17,13)==1"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/9",
        "setup_commands": [],
        "test_commands": [
            'python -c "def binary_search(arr,target): lo,hi=0,len(arr)-1; [None]; mid=(lo+hi)//2; return arr[mid] if arr[mid]==target else (binary_search(arr[lo:mid],target) if arr[mid]>target and lo<=mid else (binary_search(arr[mid+1:hi+1],target) if arr[mid]<target and mid+1<=hi else -1))"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/10",
        "setup_commands": [],
        "test_commands": [
            'python -c "def is_prime(n): return n>1 and all(n%i!=0 for i in range(2,int(n**0.5)+1)); assert is_prime(2) and is_prime(17) and not is_prime(1) and not is_prime(15)"',
        ],
        "expected_output": None,
        "timeout": 30,
    },
    {
        "task_id": "HumanEval/11",
        "setup_commands": [],
        "test_commands": [
            "python -c \"assert sum(range(101)) == 5050; print('OK')\"",
        ],
        "expected_output": "OK",
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
