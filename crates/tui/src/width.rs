//! 终端显示宽度计算与表格边框绘制：处理 ANSI 转义、CJK/全角/emoji 双列宽度，
//! 以及表格的上/中/下边框。供 `render.rs` 的 Markdown 表格与截断逻辑复用。

use crate::theme::ColorTheme;

/// 计算字符串的可见字符宽度（跳过 ANSI 转义码）
pub(crate) fn visible_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut in_esc = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if in_esc {
            if bytes[i] == b'm' {
                in_esc = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            in_esc = true;
            i += 2;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        w += char_display_width(ch);
        i += ch.len_utf8();
    }
    w
}
/// 终端列宽：East Asian Wide/Fullwidth 计 2，其余计 1。
/// 注意：箭头(→)、项目符号(•)、表格边框(│) 等符号虽码位 > 0x2000，但终端按 1 列显示，
/// 不能简单按 `c > 0x2000` 判 2 列，否则填充多算导致表格列错位。
pub(crate) fn char_display_width(c: char) -> usize {
    let u = c as u32;
    // 零宽字符：组合附加符号、组合用符号（含 keycap U+20E3）、变体选择符、ZWJ
    if u == 0
        || (0x0300..=0x036F).contains(&u)
        || (0x20D0..=0x20FF).contains(&u)
        || (0xFE00..=0xFE0F).contains(&u)
        || matches!(u, 0x200B | 0x200C | 0x200D | 0x2060)
    {
        return 0;
    }
    let wide = matches!(u,
        0x1100..=0x115F |   // 谚文 Jamo
        0x2600..=0x26FF |   // 杂项符号（⚠ ☀ ☎ 等 emoji，终端按 2 列显示）
        0x2700..=0x27BF |   // Dingbats（✅ ✂ ✈ 等 emoji，终端按 2 列显示）
        0x2E80..=0x303E |   // CJK 部首、康熙部首、CJK 符号与标点
        0x3041..=0x33FF |   // 平假名、片假名、注音、CJK 兼容
        0x3400..=0x4DBF |   // CJK 扩展 A
        0x4E00..=0x9FFF |   // CJK 统一表意文字
        0xA000..=0xA4CF |   // 彝文
        0xAC00..=0xD7A3 |   // 谚文音节
        0xF900..=0xFAFF |   // CJK 兼容表意文字
        0xFE10..=0xFE19 |   // 竖排标点
        0xFE30..=0xFE6F |   // CJK 兼容形式、小写变体
        0xFF00..=0xFF60 |   // 全角 ASCII、全角标点
        0xFFE0..=0xFFE6 |   // 全角符号
        0x1F300..=0x1FAFF | // emoji 及符号
        0x20000..=0x3FFFD   // CJK 扩展 B 及以上
    );
    if wide {
        2
    } else {
        1
    }
}

pub(crate) fn spacer(n: usize) -> String {
    " ".repeat(n)
}
/// 将字符串按可见宽度截断到 `max`（含尾部 `…`），跳过 ANSI 转义码、尊重 CJK 宽度。
/// 不含转义码且本身不超宽时原样返回；超宽时在末尾补一个宽度 1 的 `…`。
pub(crate) fn truncate_to_width(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    // 需要截断：为 `…` 预留 1 列
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    let mut in_esc = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if in_esc {
            out.push(bytes[i] as char);
            if bytes[i] == b'm' {
                in_esc = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            out.push_str("\x1b[");
            in_esc = true;
            i += 2;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        let cw = char_display_width(ch);
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
        i += ch.len_utf8();
    }
    out.push('…');
    out
}
// =============================================================================
// 表格渲染辅助
// =============================================================================

pub(crate) fn table_border_top(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('┌');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() {
            s.push('┬');
        }
    }
    s.push('┐');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}

pub(crate) fn table_border_sep(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('├');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() {
            s.push('┼');
        }
    }
    s.push('┤');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}

pub(crate) fn table_border_bottom(widths: &[usize]) -> String {
    let mut s = String::from(ColorTheme::DIM);
    s.push('└');
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        if i + 1 < widths.len() {
            s.push('┴');
        }
    }
    s.push('┘');
    s.push_str(ColorTheme::RESET);
    s.push('\n');
    s
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_width_is_one_per_char() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn cjk_chars_are_two_columns() {
        assert_eq!(visible_width("中文"), 4);
        assert_eq!(visible_width("a中b"), 4); // 1 + 2 + 1
    }

    #[test]
    fn symbols_above_0x2000_stay_one_column() {
        // 回归：旧逻辑 `c > 0x2000 即算 2 列` 会把这些误判成 2，导致表格列错位
        assert_eq!(char_display_width('→'), 1); // U+2192 箭头
        assert_eq!(char_display_width('•'), 1); // U+2022 项目符号
        assert_eq!(char_display_width('│'), 1); // U+2502 表格边框
        assert_eq!(char_display_width('—'), 1); // U+2014 破折号
        assert_eq!(char_display_width('“'), 1); // U+201C 左引号
    }

    #[test]
    fn fullwidth_and_emoji_are_two_columns() {
        assert_eq!(char_display_width('，'), 2); // U+FF0C 全角逗号
        assert_eq!(char_display_width('🦀'), 2); // emoji
    }

    #[test]
    fn dingbats_and_misc_symbols_are_two_columns() {
        // 回归：表格里的 ✅ ⚠ 等 emoji 旧逻辑算 1 列，导致边框宽度算错、列错位
        assert_eq!(char_display_width('✅'), 2); // U+2705 Dingbats
        assert_eq!(char_display_width('⚠'), 2); // U+26A0 杂项符号
        assert_eq!(char_display_width('🎯'), 2); // U+1F3AF
                                                 // 变体选择符 U+FE0F（emoji 表现）应算 0 列，不额外占位
        assert_eq!(char_display_width('\u{FE0F}'), 0);
        assert_eq!(visible_width("✅\u{FE0F}"), 2);
    }

    #[test]
    fn ansi_escapes_have_zero_width() {
        let styled = format!("{}中{}", ColorTheme::BOLD, ColorTheme::RESET);
        assert_eq!(visible_width(&styled), 2);
    }
}
