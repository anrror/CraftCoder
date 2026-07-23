"""EvalRunner — load EvalTask instances and dispatch them in parallel.

Uses :class:`concurrent.futures.ThreadPoolExecutor` so multiple tasks can
be evaluated concurrently, each inside its own Docker container.
"""

from __future__ import annotations

import logging
from concurrent.futures import Future, ThreadPoolExecutor, as_completed
from typing import Sequence

from code_agent_eval.models import EvalResult, EvalStatus, EvalTask
from code_agent_eval.sandbox import DEFAULT_IMAGE, DockerSandbox

logger = logging.getLogger(__name__)


class EvalRunner:
    """High-level orchestrator: load tasks → run in Docker → collect results.

    Parameters:
        image:     Docker image used for sandboxes.
        max_workers: Maximum number of parallel sandboxes.
    """

    def __init__(
        self,
        image: str = DEFAULT_IMAGE,
        max_workers: int = 4,
    ) -> None:
        self._image = image
        self._max_workers = max_workers

    # ------------------------------------------------------------------
    # single task
    # ------------------------------------------------------------------
    def run_single(self, task: EvalTask) -> EvalResult:
        """Run *one* task synchronously and return its result."""
        return self._evaluate_one(task)

    # ------------------------------------------------------------------
    # batch (parallel)
    # ------------------------------------------------------------------
    def run_batch(self, tasks: Sequence[EvalTask]) -> list[EvalResult]:
        """Run *tasks* in parallel and return results in submission order.

        Each task gets its own :class:`DockerSandbox`.
        """
        results: list[EvalResult] = [self._placeholder(task) for task in tasks]
        future_map: dict[Future[EvalResult], int] = {}

        with ThreadPoolExecutor(max_workers=self._max_workers) as pool:
            for idx, task in enumerate(tasks):
                future_map[pool.submit(self._evaluate_one, task)] = idx

            for future in as_completed(future_map):
                idx = future_map[future]
                try:
                    results[idx] = future.result()
                except Exception:
                    logger.exception("eval failed for task %s", tasks[idx].task_id)
                    results[idx] = EvalResult(
                        task_id=tasks[idx].task_id,
                        status=EvalStatus.ERROR,
                        score=0.0,
                        logs="internal runner error",
                    )

        return results

    # ------------------------------------------------------------------
    # internal
    # ------------------------------------------------------------------
    def _evaluate_one(self, task: EvalTask) -> EvalResult:
        sandbox = DockerSandbox(image=self._image)
        sandbox_result = sandbox.run(task)

        logs = self._build_logs(sandbox_result)
        status, score = self._score(task, sandbox_result)

        return EvalResult(
            task_id=task.task_id,
            status=status,
            score=score,
            logs=logs,
        )

    # ------------------------------------------------------------------
    # helpers
    # ------------------------------------------------------------------
    @staticmethod
    def _build_logs(sr: "SandboxResult") -> str:  # noqa: F821
        parts = [sr.stdout]
        if sr.stderr:
            parts.append(f"--- STDERR ---\n{sr.stderr}")
        return "\n".join(parts)

    @staticmethod
    def _score(task: EvalTask, sr: "SandboxResult") -> tuple[EvalStatus, float]:  # noqa: F821
        if sr.timed_out:
            return EvalStatus.ERROR, 0.0
        if sr.exit_code != 0:
            return EvalStatus.FAIL, 0.0

        # If an expected output is provided, do a loose substring match.
        if task.expected_output is not None:
            normalised = " ".join(sr.stdout.split())
            expected = " ".join(task.expected_output.split())
            if expected in normalised:
                return EvalStatus.PASS, 1.0
            return EvalStatus.FAIL, 0.0

        # Exit code 0 with no expected output → PASS.
        return EvalStatus.PASS, 1.0

    @staticmethod
    def _placeholder(task: EvalTask) -> EvalResult:
        return EvalResult(task_id=task.task_id, status=EvalStatus.ERROR)
