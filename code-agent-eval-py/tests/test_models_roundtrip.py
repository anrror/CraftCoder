"""Round-trip tests for EvalTask and EvalResult JSON serialisation.

These tests must pass WITHOUT Docker (pure data model tests).
"""

from __future__ import annotations

import pytest

from code_agent_eval.models import EvalResult, EvalStatus, EvalTask, deserialize_result, deserialize_task, serialize_result


class TestEvalTaskRoundTrip:
    """EvalTask → JSON → EvalTask."""

    def test_minimal_task(self) -> None:
        task = EvalTask(task_id="HumanEval/0", test_commands=["python -m pytest"])
        json_str = task.model_dump_json()
        restored = deserialize_task(json_str)
        assert restored == task
        assert restored.task_id == "HumanEval/0"
        assert restored.timeout == 120  # default

    def test_full_task(self) -> None:
        task = EvalTask(
            task_id="test/full",
            setup_commands=["pip install requests"],
            test_commands=["python test.py"],
            expected_output="OK",
            timeout=42,
        )
        json_str = task.model_dump_json()
        restored = deserialize_task(json_str)
        assert restored == task
        assert restored.expected_output == "OK"
        assert restored.timeout == 42

    def test_task_is_frozen(self) -> None:
        task = EvalTask(task_id="x", test_commands=["echo"])
        with pytest.raises(Exception):  # pydantic ValidationError or FrozenInstanceError
            task.timeout = 999  # type: ignore[misc]


class TestEvalResultRoundTrip:
    """EvalResult → JSON → EvalResult."""

    def test_pass_result(self) -> None:
        r = EvalResult(task_id="t1", status=EvalStatus.PASS, score=1.0, logs="all good")
        j = serialize_result(r)
        restored = EvalResult.deserialize_from_json(j)
        assert restored == r
        assert restored.score == 1.0

    def test_error_result(self) -> None:
        r = EvalResult(task_id="t2", status=EvalStatus.ERROR, score=0.0, logs="timeout")
        j = r.serialize_to_json()
        restored = EvalResult.deserialize_from_json(j)
        assert restored.status == EvalStatus.ERROR
        assert restored.score == 0.0

    def test_fail_result_with_patch(self) -> None:
        r = EvalResult(
            task_id="t3",
            status=EvalStatus.FAIL,
            score=0.3,
            logs="3/10 tests passed",
            patch="diff --git a/x.py b/x.py",
        )
        j = serialize_result(r)
        restored = deserialize_result(j)
        assert restored.patch == "diff --git a/x.py b/x.py"


class TestStatusEnum:
    """EvalStatus enum values round-trip through JSON."""

    def test_all_variants(self) -> None:
        for status in EvalStatus:
            r = EvalResult(task_id="x", status=status)
            j = r.serialize_to_json()
            restored = deserialize_result(j)
            assert restored.status == status
