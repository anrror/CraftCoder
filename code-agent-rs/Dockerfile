# syntax=docker/dockerfile:1
# ============================================================
# Code Agent — 多阶段 Rust 构建镜像
# ============================================================
# 构建用法:
#   docker build -t code-agent:latest -f code-agent-rs/Dockerfile code-agent-rs
#
# 运行:
#   docker run --rm -e LLM_API_KEY=sk-... -e LLM_CHAT_MODEL=qwen3.6-27b \
#     -p 3000:3000 code-agent:latest web
#
# 可用入口:
#   web        — code-agent-web (Axum HTTP, 默认端口 3000)
#   app-server — code-agent-app-server (JSON-RPC over stdio)
# ============================================================

# ── Stage 1: Builder ────────────────────────────────────────
FROM rust:1.80-slim-bookworm AS builder

# 系统依赖: libsqlite3 用于 rusqlite(system=bundled 不需要)，但保留 git/ssl 用于 cargo
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 先复制依赖清单，利用 Docker 缓存层
COPY Cargo.toml Cargo.lock ./
COPY protocol/Cargo.toml protocol/
COPY core/Cargo.toml core/
COPY tools/Cargo.toml tools/
COPY codex/Cargo.toml codex/
COPY eval/Cargo.toml eval/
COPY cli/Cargo.toml cli/
COPY app-server/Cargo.toml app-server/
COPY code-agent-web/Cargo.toml code-agent-web/
COPY eval-runner/Cargo.toml eval-runner/

# 预构建依赖（创建伪 main.rs 以编译依赖树）
RUN mkdir -p protocol/src core/src tools/src codex/src eval/src cli/src app-server/src code-agent-web/src eval-runner/src && \
    echo "fn main() {}" > protocol/src/lib.rs && \
    echo "fn main() {}" > core/src/lib.rs && \
    echo "fn main() {}" > tools/src/lib.rs && \
    echo "fn main() {}" > codex/src/lib.rs && \
    echo "fn main() {}" > eval/src/lib.rs && \
    echo "fn main() {}" > cli/src/main.rs && \
    echo "fn main() {}" > app-server/src/main.rs && \
    echo "fn main() {}" > code-agent-web/src/main.rs && \
    echo "fn main() {}" > eval-runner/src/main.rs && \
    cargo build --release -p code-agent-web -p code-agent-app-server -p code-agent-cli 2>/dev/null || true

# 复制真实源码
COPY protocol/src protocol/src/
COPY core/src core/src/
COPY tools/src tools/src/
COPY codex/src codex/src/
COPY eval/src eval/src/
COPY cli/src cli/src/
COPY app-server src app-server/src/
COPY code-agent-web/src code-agent-web/src/
COPY code-agent-web/static code-agent-web/static/
COPY eval-runner/src eval-runner/src/

# 完整构建
RUN cargo build --release -p code-agent-web -p code-agent-app-server -p code-agent-cli

# ── Stage 2: Runtime ─────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 从 builder 复制二进制和静态文件
COPY --from=builder /app/target/release/code-agent-web /usr/local/bin/
COPY --from=builder /app/target/release/code-agent-app-server /usr/local/bin/
COPY --from=builder /app/target/release/code-agent-cli /usr/local/bin/
COPY --from=builder /app/code-agent-web/static /opt/code-agent-web/static

# 创建非 root 用户
RUN addgroup --system --gid 1001 codeagent && \
    adduser --system --uid 1001 --ingroup codeagent --home /home/codeagent codeagent && \
    chown -R codeagent:codeagent /opt/code-agent-web

USER codeagent
WORKDIR /home/codeagent

# Web 服务默认端口
EXPOSE 3000

# 默认入口（可通过 docker run 覆盖）
ENTRYPOINT ["/usr/local/bin/code-agent-web"]
CMD ["--host", "0.0.0.0", "--port", "3000", "--static-dir", "/opt/code-agent-web/static"]
