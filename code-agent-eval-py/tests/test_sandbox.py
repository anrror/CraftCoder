"""Smoke-test for DockerSandbox — skipped when Docker daemon is unavailable."""

from __future__ import annotations

import shutil
import subprocess

import pytest

from code_agent_eval.models import EvalTask
from code_agent_eval.sandbox import DockerSandbox


def _docker_daemon_reachable() -> bool:
    """Return True if the Docker CLI + daemon are both available."""
    if shutil.which("docker") is None:
        return False
    try:
        result = subprocess.run(
            ["docker", "info"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return result.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


_docker_ok = _docker_daemon_reachable()
skip_docker = pytest.mark.skipif(not _docker_ok, reason="Docker daemon not reachable")


@skip_docker
def test_hello_world() -> None:
    """A trivial echo ensures the sandbox works end-to-end."""
    task = EvalTask(
        task_id="smoke/hello",
        test_commands=["echo hello world"],
        timeout=30,
    )
    sandbox = DockerSandbox()
    result = sandbox.run(task)

    assert result.exit_code == 0, f"stderr: {result.stderr}"
    assert "hello world" in result.stdout
    assert not result.timed_out


@skip_docker
def test_exit_code_propagation() -> None:
    """Non-zero exit from the container should be captured."""
    task = EvalTask(
        task_id="smoke/fail",
        test_commands=["exit 42"],
        timeout=30,
    )
    sandbox = DockerSandbox()
    result = sandbox.run(task)

    assert result.exit_code == 42


@skip_docker
def test_setup_then_test() -> None:
    """Setup commands run before test commands."""
    task = EvalTask(
        task_id="smoke/setup",
        setup_commands=["echo 'setup done' > /tmp/marker"],
        test_commands=["cat /tmp/marker"],
        timeout=30,
    )
    sandbox = DockerSandbox()
    result = sandbox.run(task)

    assert result.exit_code == 0
    assert "setup done" in result.stdout


@skip_docker
def test_context_manager() -> None:
    """DockerSandbox works as a context manager."""
    with DockerSandbox() as sandbox:
        task = EvalTask(
            task_id="smoke/ctx",
            test_commands=["echo ok"],
            timeout=30,
        )
        result = sandbox.run(task)
        assert result.exit_code == 0
