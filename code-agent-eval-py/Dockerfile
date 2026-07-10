# Dockerfile for the Code Agent Eval environment.
#
# Build:
#   docker build -t code-agent-eval:latest -f code-agent-eval-py/Dockerfile .
#
# This image is the default execution environment for SWE-bench-Live /
# HumanEval evaluation tasks.

FROM python:3.12-slim

# System utilities needed by eval harnesses
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git \
        jq \
        curl \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the eval package into the image
COPY code-agent-eval-py /opt/code-agent-eval-py

# Install the package in editable mode so changes are picked up
RUN pip install --no-cache-dir /opt/code-agent-eval-py

WORKDIR /workspace

# Default: run the CLI
ENTRYPOINT ["code-agent-eval"]
CMD ["--help"]
