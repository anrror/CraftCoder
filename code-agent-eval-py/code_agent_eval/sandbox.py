"""Docker-based sandbox for isolated evaluation.

Each :class:`DockerSandbox` manages a single ``docker run`` container with
resource constraints and a hard timeout.
"""

from __future__ import annotations

import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from code_agent_eval.models import EvalTask

# ---------------------------------------------------------------------------
# Resource defaults (see spec: --network none --memory 4g --cpus 2)
# ---------------------------------------------------------------------------
DEFAULT_IMAGE = "python:3.12-slim"
DEFAULT_MEMORY = "4g"
DEFAULT_CPUS = "2"
DEFAULT_NETWORK = "none"


@dataclass
class SandboxResult:
    """Result from a single sandbox run."""

    exit_code: int
    stdout: str
    stderr: str
    timed_out: bool = False
    elapsed: float = 0.0


@dataclass
class DockerSandbox:
    """Manage an ephemeral Docker container for a single eval task.

    Usage::

        sandbox = DockerSandbox(image="python:3.12-slim")
        result = sandbox.run(task)

    The container is created on first ``run()`` and destroyed on ``cleanup()``
    (or via the context-manager protocol).
    """

    image: str = DEFAULT_IMAGE
    memory: str = DEFAULT_MEMORY
    cpus: str = DEFAULT_CPUS
    network: str = DEFAULT_NETWORK
    _work_dir: Path = field(default_factory=lambda: Path("/tmp/code-agent-eval"))

    # ------------------------------------------------------------------
    # low-level: execute a command list inside a disposable container
    # ------------------------------------------------------------------
    def _execute_raw(
        self,
        commands: list[str],
        timeout: int,
        env: dict[str, str] | None = None,
    ) -> SandboxResult:
        """Run *commands* inside a fresh ``docker run`` container and return
        the captured output.

        The container is automatically removed on exit (``--rm``).
        """
        # Build a shell script that runs the commands sequentially,
        # failing-fast on the first non-zero exit.
        script_lines = ["set -e", "set -o pipefail", *commands]
        script = "\n".join(script_lines)

        docker_args = [
            "docker",
            "run",
            "--rm",
            "--network",
            self.network,
            "--memory",
            self.memory,
            "--cpus",
            self.cpus,
            "--workdir",
            str(self._work_dir),
        ]
        if env:
            for k, v in env.items():
                docker_args.extend(["-e", f"{k}={v}"])

        docker_args.extend([self.image, "bash", "-c", script])

        started = time.monotonic()

        try:
            proc = subprocess.run(
                docker_args,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            elapsed = time.monotonic() - started
            return SandboxResult(
                exit_code=proc.returncode,
                stdout=proc.stdout,
                stderr=proc.stderr,
                timed_out=False,
                elapsed=elapsed,
            )
        except subprocess.TimeoutExpired:
            elapsed = time.monotonic() - started
            return SandboxResult(
                exit_code=-1,
                stdout="",
                stderr="command timed out",
                timed_out=True,
                elapsed=elapsed,
            )

    # ------------------------------------------------------------------
    # high-level: run a full EvalTask
    # ------------------------------------------------------------------
    def run(self, task: EvalTask) -> SandboxResult:
        """Execute *setup_commands* then *test_commands* in sequence.

        Returns a :class:`SandboxResult` for downstream scoring.
        """
        all_commands = [*task.setup_commands, *task.test_commands]
        return self._execute_raw(all_commands, timeout=task.timeout)

    # -- context manager --------------------------------------------------
    def cleanup(self) -> None:
        """No-op (containers are auto-removed with ``--rm``)."""

    def __enter__(self) -> DockerSandbox:
        return self

    def __exit__(self, *args: object) -> None:
        self.cleanup()
