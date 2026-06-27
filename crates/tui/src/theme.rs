//! 颜色主题、围栏标记 — 跨模块共享的展示原语。

// =============================================================================
// 颜色主题 — 集中管理所有 ANSI 颜色，一处修改全局生效
// =============================================================================

pub(crate) struct ColorTheme;

impl ColorTheme {
    pub(crate) const DIM: &'static str = "\x1b[90m";
    pub(crate) const RESET: &'static str = "\x1b[0m";
    pub(crate) const BOLD: &'static str = "\x1b[1m";
    pub(crate) const ITALIC: &'static str = "\x1b[3m";
    pub(crate) const UNDERLINE: &'static str = "\x1b[4m";
    pub(crate) const ACCENT: &'static str = "\x1b[36m"; // 青色：工具调用、引用、标题
    pub(crate) const ERROR: &'static str = "\x1b[31m"; // 红色：错误
    pub(crate) const SUCCESS: &'static str = "\x1b[32m"; // 绿色：成功
    pub(crate) const CODE_INLINE: &'static str = "\x1b[33m"; // 黄色：行内代码
    pub(crate) const BULLET: &'static str = "\x1b[33m"; // 黄色：列表符号
    pub(crate) const HEADING_H1: &'static str = "\x1b[1;36m";
    pub(crate) const HEADING_H2: &'static str = "\x1b[1;34m";
    pub(crate) const HEADING_H3: &'static str = "\x1b[1;35m";
    pub(crate) const QUOTE: &'static str = "\x1b[36m";
    pub(crate) const STRIKETHROUGH: &'static str = "\x1b[9m";
    pub(crate) const CODE_BG: &'static str = "\x1b[48;5;236m"; // 代码块深色背景
    pub(crate) const LINK: &'static str = "\x1b[34m"; // 蓝色：链接
}

// =============================================================================
// 围栏标记 — 用于流式渲染中追踪未闭合代码围栏
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FenceMarker {
    /// 围栏字符：'`' (backtick) 或 '~' (tilde)
    pub(crate) character: char,
    /// 围栏长度（最少 3）
    pub(crate) length: usize,
}
