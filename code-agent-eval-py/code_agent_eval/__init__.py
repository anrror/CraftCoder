"""Code Agent Eval — Python evaluation environment for the AI Coding Agent.

Supports running SWE-bench-Live and HumanEval evaluations via Docker sandboxes,
with a PyO3 Rust bridge for integration with the `code-agent-rs` agent core.
"""

from code_agent_eval.models import EvalTask, EvalResult
from code_agent_eval.runner import EvalRunner
from code_agent_eval.sandbox import DockerSandbox

__all__ = [
    "DockerSandbox",
    "EvalResult",
    "EvalRunner",
    "EvalTask",
]
__version__ = "0.1.0"
