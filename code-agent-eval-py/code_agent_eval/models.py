"""Pydantic v2 data models for the eval system.

EvalTask:  input — what to evaluate.
EvalResult: output — how it went.
"""

from __future__ import annotations

from enum import Enum

from pydantic import BaseModel, Field


class EvalStatus(str, Enum):
    """Outcome of a single eval task."""

    PASS = "pass"
    FAIL = "fail"
    ERROR = "error"


class EvalTask(BaseModel):
    """A single evaluation task — re-usable, serialisable.

    Attributes:
        task_id:        Unique identifier (e.g. ``HumanEval/0``).
        setup_commands: Shell commands to prepare the sandbox.
        test_commands:  Shell commands that constitute the actual test.
        expected_output:If set, the test output is compared against this
                        string (loose match — substring / normalised whitespace).
        timeout:        Maximum wall-clock seconds for the entire task.
    """

    task_id: str
    setup_commands: list[str] = Field(default_factory=list)
    test_commands: list[str]
    expected_output: str | None = None
    timeout: int = Field(default=120, ge=1, description="Timeout in seconds")

    model_config = {"frozen": True}


class EvalResult(BaseModel):
    """The result of running a single :class:`EvalTask`.

    Attributes:
        task_id: Mirrors the input task id.
        status:  ``pass`` / ``fail`` / ``error``.
        score:   Numeric score (0.0 – 1.0).  1.0 = perfect pass.
        logs:    Combined stdout + stderr from the sandbox.
        patch:   Optional diff / code change produced during evaluation.
    """

    task_id: str
    status: EvalStatus
    score: float = Field(default=0.0, ge=0.0, le=1.0)
    logs: str = ""
    patch: str | None = None

    def serialize_to_json(self) -> str:
        """Return a compact JSON representation (no extra whitespace).

        Used by the Rust PyO3 bridge to pass results across the FFI boundary.
        """
        return self.model_dump_json()

    @classmethod
    def deserialize_from_json(cls, data: str) -> EvalResult:
        """Reconstruct an ``EvalResult`` from the JSON produced by
        :meth:`serialize_to_json`.
        """
        return cls.model_validate_json(data)


# Aliases that the Rust bridge can call without importing Pydantic internals.
serialize_result = EvalResult.serialize_to_json
deserialize_result = EvalResult.deserialize_from_json

deserialize_task = EvalTask.model_validate_json
