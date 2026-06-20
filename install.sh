#!/bin/bash
# Raven 🐦‍⬛ 一键安装脚本
# 支持: Linux x86_64, macOS x86_64/arm64

set -e

REPO="yourname/raven"
BINARY_NAME="raven"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# 颜色
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_ok() {
    echo -e "${GREEN}[OK]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 检测操作系统
detect_os() {
    local os=""
    local arch=""

    case "$(uname -s)" in
        Linux*)     os="linux";;
        Darwin*)    os="darwin";;
        CYGWIN*|MINGW*|MSYS*) os="windows";;
        *)          log_error "不支持的操作系统: $(uname -s)"; exit 1;;
    esac

    case "$(uname -m)" in
        x86_64)   arch="x86_64";;
        amd64)    arch="x86_64";;
        arm64)    arch="aarch64";;
        aarch64)  arch="aarch64";;
        *)        log_error "不支持的架构: $(uname -m)"; exit 1;;
    esac

    echo "${os}_${arch}"
}

# 检查依赖
check_deps() {
    log_info "检查依赖..."

    # 检查 curl
    if ! command -v curl &> /dev/null; then
        log_error "需要 curl，请先安装"
        exit 1
    fi

    # 检查 Rust（如果从源码编译）
    if [ "$BUILD_FROM_SOURCE" = "1" ]; then
        if ! command -v cargo &> /dev/null; then
            log_warn "未找到 Rust，将尝试安装..."
            install_rust
        fi
    fi
}

# 安装 Rust
install_rust() {
    log_info "安装 Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    log_ok "Rust 安装完成"
}

# 从 GitHub Release 下载
install_from_release() {
    local platform="$1"
    local version="${VERSION:-latest}"

    if [ "$version" = "latest" ]; then
        local url="https://github.com/${REPO}/releases/latest/download/${BINARY_NAME}_${platform}.tar.gz"
    else
        local url="https://github.com/${REPO}/releases/download/${version}/${BINARY_NAME}_${platform}.tar.gz"
    fi

    log_info "下载 ${platform} 版本..."

    local tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    if curl -fsSL "$url" -o "$tmp_dir/raven.tar.gz" 2>/dev/null; then
        tar -xzf "$tmp_dir/raven.tar.gz" -C "$tmp_dir"
        mv "$tmp_dir/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        chmod +x "$INSTALL_DIR/$BINARY_NAME"
        log_ok "下载完成"
    else
        log_warn "下载 Release 失败，将尝试从源码编译..."
        install_from_source
    fi
}

# 从源码编译
install_from_source() {
    log_info "从源码编译..."

    local tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    log_info "克隆仓库..."
    git clone --depth 1 "https://github.com/${REPO}.git" "$tmp_dir/raven"

    cd "$tmp_dir/raven"

    log_info "编译 Release 版本..."
    cargo build --release

    cp "target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"

    log_ok "编译完成"
}

# 创建配置目录
setup_config() {
    log_info "创建配置目录..."
    mkdir -p "$HOME/.raven"

    if [ ! -f "$HOME/.raven/config.toml" ]; then
        cat > "$HOME/.raven/config.toml" << 'EOF'
# Raven 🐦‍⬛ 配置文件
# 文档: https://github.com/yourname/raven#配置

model = "gpt-4o"

[permission]
mode = "ask"
allowed_tools = ["file_read", "file_write", "file_edit", "view", "search", "list_dir", "git", "web_search", "fetch_url"]

[context]
max_tokens = 128000
compact_threshold = 100000
keep_rounds = 6
EOF
        log_ok "配置文件已创建: ~/.raven/config.toml"
    fi

    # 创建会话目录
    mkdir -p "$HOME/.raven/sessions"
    mkdir -p "$HOME/.raven/checkpoints"
}

# 添加到 PATH
add_to_path() {
    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        log_warn "$INSTALL_DIR 不在 PATH 中"
        echo ""
        echo "请手动添加以下行到你的 shell 配置文件:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        echo ""
    fi
}

# 验证安装
verify() {
    if command -v "$INSTALL_DIR/$BINARY_NAME" &> /dev/null; then
        echo ""
        log_ok "安装成功!"
        echo ""
        echo "使用方法:"
        echo "  raven --help           # 显示帮助"
        echo "  raven \"你的问题\"      # 单次提问"
        echo "  raven chat             # 交互式对话"
        echo "  raven tui              # TUI 全屏界面"
        echo "  raven serve            # 启动 HTTP API"
        echo ""
        echo "设置 API Key:"
        echo "  export RAVEN_API_KEY=sk-..."
        echo ""
        return 0
    else
        log_error "安装失败"
        return 1
    fi
}

# 主函数
main() {
    echo "=========================================="
    echo "  Raven 🐦‍⬛ 安装脚本"
    echo "=========================================="
    echo ""

    # 解析参数
    BUILD_FROM_SOURCE="0"
    while [ $# -gt 0 ]; do
        case "$1" in
            --source) BUILD_FROM_SOURCE="1"; shift;;
            --version) VERSION="$2"; shift 2;;
            --dir) INSTALL_DIR="$2"; shift 2;;
            *) shift;;
        esac
    done

    # 创建安装目录
    mkdir -p "$INSTALL_DIR"

    local platform=$(detect_os)
    log_info "检测平台: $platform"
    log_info "安装目录: $INSTALL_DIR"

    check_deps

    if [ "$BUILD_FROM_SOURCE" = "1" ]; then
        install_from_source
    else
        install_from_release "$platform"
    fi

    setup_config
    add_to_path
    verify
}

main "$@"
