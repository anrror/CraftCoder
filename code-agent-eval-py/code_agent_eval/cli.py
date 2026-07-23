"""CLI entry-point for ``code-agent-eval`` (click-based).

Registered as ``code-agent-eval`` console_script in pyproject.toml.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import click

from code_agent_eval.models import EvalTask
from code_agent_eval.runner import EvalRunner


@click.group()
def main() -> None:
    """Code Agent Eval CLI — run eval tasks inside Docker sandboxes."""


@main.command()
@click.argument("tasks_file", type=click.Path(exists=True))
@click.option(
    "--image",
    default="python:3.12-slim",
    show_default=True,
    help="Docker image for sandboxes.",
)
@click.option(
    "--max-workers",
    default=4,
    show_default=True,
    help="Max parallel sandboxes.",
)
@click.option(
    "--output",
    "-o",
    type=click.Path(),
    default=None,
    help="Write JSON results to this file (stdout if omitted).",
)
def run(tasks_file: str, image: str, max_workers: int, output: str | None) -> None:
    """Load tasks from a JSON file and run them all.

    TASKS_FILE must be a JSON array of EvalTask objects:

    \b
    [
      {
        "task_id": "hello",
        "setup_commands": [],
        "test_commands": ["echo hello"],
        "expected_output": "hello",
        "timeout": 30
      }
    ]
    """
    tasks = _load_tasks(Path(tasks_file))
    runner = EvalRunner(image=image, max_workers=max_workers)
    results = runner.run_batch(tasks)

    payload: list[dict[str, Any]] = [r.model_dump() for r in results]
    json_str = json.dumps(payload, indent=2)

    if output:
        Path(output).write_text(json_str, encoding="utf-8")
        click.echo(f"Results written to {output}")
    else:
        click.echo(json_str)


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------
def _load_tasks(path: Path) -> list[EvalTask]:
    raw = path.read_text(encoding="utf-8")
    data = json.loads(raw)
    if not isinstance(data, list):
        msg = "tasks_file must contain a JSON array of EvalTask objects"
        raise click.BadParameter(msg)
    return [EvalTask.model_validate(obj) for obj in data]


if __name__ == "__main__":
    main()
