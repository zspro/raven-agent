//! 通用工具函数

/// 简单估算文本的 token 数
/// 中文 ≈ 1 字 1 token，英文 ≈ 4 字符 1 token
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let (chinese, other) = text.chars().fold((0, 0), |(c, o), ch| {
        if ch as u32 > 127 {
            (c + 1, o)
        } else {
            (c, o + 1)
        }
    });

    chinese + other / 4 + 1
}

/// 截断过长字符串。
///
/// 注意：`max_chars` 语义为「最多保留的字符数」，按 Unicode 字符（而非字节）
/// 截断，避免在多字节 UTF-8 字符（如中文）中间切断导致 panic。
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{}\n... [已截断，共 {} 字符]", head, text.chars().count())
    }
}
