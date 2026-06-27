# Raven 🐦‍⬛ Docker 镜像
# 多阶段构建，最终镜像基于 debian:slim

# =============================================================================
# 构建阶段
# =============================================================================
# 基础镜像的 Rust 版本必须满足整个依赖树的 MSRV。当前 Cargo.lock 锁定的
# 依赖中，最高要求来自 plist / time / time-core（需 rustc >= 1.88），其次是
# icu_* / idna_adapter（需 1.86）；根 Cargo.toml 用 resolver = "3"（需 >= 1.84）、
# Cargo.lock 为 v4（需 >= 1.78）。
# 使用滚动 tag `rust:1-...`（始终为最新 1.x 稳定版），与 CI 的
# `dtolnay/rust-toolchain@stable` 策略一致，避免日后依赖抬高 MSRV 时再次失效。
FROM rust:1-slim-bookworm AS builder

WORKDIR /build

# 复制全部源码后直接构建。
# 项目使用 rustls（见 Cargo.toml 的 reqwest 特性），不依赖系统 OpenSSL，
# 因此无需 pkg-config / libssl-dev。
COPY . .

# 编译 Release 版本
RUN cargo build --release -p cli

# =============================================================================
# 运行阶段
# =============================================================================
FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="Raven"
LABEL org.opencontainers.image.description="A sharp, cross-platform AI agent in Rust"
LABEL org.opencontainers.image.source="https://github.com/yourname/raven"

WORKDIR /app

# 安装运行时依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    && rm -rf /var/lib/apt/lists/*

# 从构建阶段复制二进制
COPY --from=builder /build/target/release/raven /usr/local/bin/raven

# Web UI 静态文件
COPY --from=builder /build/web /app/web

# 创建配置目录
RUN mkdir -p /root/.raven

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD raven doctor || exit 1

# 默认端口
EXPOSE 8080

# 入口点
ENTRYPOINT ["raven"]
CMD ["--help"]
