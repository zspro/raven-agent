# Raven 🐦‍⬛ Docker 镜像
# 多阶段构建，最终镜像基于 debian:slim

# =============================================================================
# 构建阶段
# =============================================================================
FROM rust:1.76-slim-bookworm AS builder

WORKDIR /build

# 安装编译依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 复制依赖定义（利用 Docker 缓存层）
COPY Cargo.toml Cargo.lock ./
COPY crates/*/Cargo.toml ./

# 复制源码
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
