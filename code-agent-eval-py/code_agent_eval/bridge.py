"""Thin Python-side bridge functions called by the Rust PyO3 layer.

These are deliberately kept as free functions (not class methods) so the
Rust side can call them with minimal FFI ceremony.
"""

from __future__ import annotations

import json
from typing import Any

from code_agent_eval.models import EvalResult, EvalTask
from code_agent_eval.runner import EvalRunner


def run_single_py(task_json: str) -> str:
    """Run a single EvalTask (JSON → JSON).

    Called by ``eval_bridge::run_eval``.
    """
    task = EvalTask.model_validate_json(task_json)
    runner = EvalRunner()
    result: EvalResult = runner.run_single(task)
    return result.serialize_to_json()


def run_batch_py(tasks_json: str) -> str:
    """Run a batch of EvalTasks (JSON array → JSON array).

    Called by ``eval_bridge::run_benchmark``.
    """
    raw: list[dict[str, Any]] = json.loads(tasks_json)
    tasks = [EvalTask.model_validate(obj) for obj in raw]
    runner = EvalRunner()
    results: list[EvalResult] = runner.run_batch(tasks)
    payload = [r.model_dump(mode="json") for r in results]
    return json.dumps(payload)
