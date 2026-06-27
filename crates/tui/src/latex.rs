//! 渲染前预处理：HTML `<sub>`/`<sup>` + LaTeX 数学 `$...$`/`$$...$$`/`\(...\)`/`\[...\]`
//! → Unicode 上下标，让化学方程式、数学公式在纯终端里也能可读。

use crate::latex_unicode::{
    all_subscriptable, all_superscriptable, latex_to_unicode, sub_to_unicode, sup_to_unicode,
};

/// 预处理：HTML `<sub>`/`<sup>` + LaTeX `$...$`/`$$...$$` 数学 → Unicode 上下标
///
/// 化学方程式示例：
///   H<sub>2</sub>O            → H₂O
///   $2H_2 + O_2 \rightarrow 2H_2O$ → 2H₂ + O₂ → 2H₂O
pub(crate) fn preprocess_chemistry(text: &str) -> String {
    // 第一步：HTML 标签
    let text = preprocess_html_sub_sup(text);
    // 第二步：LaTeX 数学（$...$ 内部）
    let text = preprocess_latex_math(&text);
    // 第三步：独立 \command（$...$ 外面的 \boxed{...}, \quad, \; 等）
    preprocess_standalone_commands(&text)
}

/// 处理 $...$ 外部的独立 LaTeX 命令（\boxed, \quad, \; 等）
fn preprocess_standalone_commands(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            // 空格类: \  \quad \qquad \; \, \: \!
            if next == ' ' || matches!(next, ';' | ',' | ':' | '!') {
                out.push(' ');
                i += 2;
                continue;
            }
            if next.is_alphabetic() {
                let cmd_start = i + 1;
                let mut j = cmd_start;
                while j < chars.len() && chars[j].is_alphabetic() {
                    j += 1;
                }
                let cmd: String = chars[cmd_start..j].iter().collect();

                match cmd.as_str() {
                    // 空格类
                    "quad" | "qquad" => {
                        out.push(' ');
                        i = j;
                        continue;
                    }
                    // 无操作（忽略）
                    "displaystyle" | "textstyle" | "scriptstyle" => {
                        i = j;
                        continue;
                    }
                    // \boxed{...} → 提取内容
                    "boxed" => {
                        if j < chars.len() && chars[j] == '{' {
                            let mut depth = 1u32;
                            let mut k = j + 1;
                            while k < chars.len() && depth > 0 {
                                if chars[k] == '{' {
                                    depth += 1;
                                }
                                if chars[k] == '}' {
                                    depth -= 1;
                                }
                                if depth > 0 {
                                    k += 1;
                                }
                            }
                            let inner: String = chars[j + 1..k].iter().collect();
                            // 递归处理内部内容
                            let processed = preprocess_standalone_commands(&inner);
                            out.push_str(&processed);
                            i = k + 1; // skip }
                            continue;
                        }
                        i = j;
                        continue;
                    }
                    // 文本模式: \text{...}, \mathrm{...} → 提取内容
                    "text" | "mathrm" | "mathbf" | "mathcal" | "mathit" | "mathsf" | "mathtt" => {
                        if j < chars.len() && chars[j] == '{' {
                            let mut depth = 1u32;
                            let mut k = j + 1;
                            while k < chars.len() && depth > 0 {
                                if chars[k] == '{' {
                                    depth += 1;
                                }
                                if chars[k] == '}' {
                                    depth -= 1;
                                }
                                if depth > 0 {
                                    k += 1;
                                }
                            }
                            let inner: String = chars[j + 1..k].iter().collect();
                            let processed = preprocess_standalone_commands(&inner);
                            out.push_str(&processed);
                            i = k + 1;
                            continue;
                        }
                        i = j;
                        continue;
                    }
                    _ => {
                        // 未知命令，保留原样
                        out.push('\\');
                        out.push_str(&cmd);
                        i = j;
                        continue;
                    }
                }
            }
            // 非字母非空格的 \X → 原样保留
            out.push(chars[i]);
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}
fn preprocess_html_sub_sup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 5 <= bytes.len() && &bytes[i..i + 5] == b"<sub>" {
            i += 5;
            let start = i;
            while i + 6 <= bytes.len() && &bytes[i..i + 6] != b"</sub>" {
                i += 1;
            }
            let content = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            if all_subscriptable(content) {
                out.push_str(&sub_to_unicode(content));
            } else {
                out.push_str(&format!("_({content})"));
            }
            if i + 6 <= bytes.len() {
                i += 6;
            }
        } else if i + 5 <= bytes.len() && &bytes[i..i + 5] == b"<sup>" {
            i += 5;
            let start = i;
            while i + 6 <= bytes.len() && &bytes[i..i + 6] != b"</sup>" {
                i += 1;
            }
            let content = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            if all_superscriptable(content) {
                out.push_str(&sup_to_unicode(content));
            } else {
                out.push_str(&format!("^({content})"));
            }
            if i + 6 <= bytes.len() {
                i += 6;
            }
        } else {
            let ch = text[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}
/// 扫描 `$...$` / `$$...$$` / `\(...\)` / `\[...\]` 并转换 LaTeX 数学 → Unicode
fn preprocess_latex_math(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // 检测 \[ 块 (LaTeX display math)
        if i + 2 <= len && &bytes[i..i + 2] == b"\\[" {
            let start = i + 2;
            if let Some(end) = text[start..].find("\\]") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 \( 行内 (LaTeX inline math)
        if i + 2 <= len && &bytes[i..i + 2] == b"\\(" {
            let start = i + 2;
            if let Some(end) = text[start..].find("\\)") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 $$ 块
        if i + 2 <= len && &bytes[i..i + 2] == b"$$" {
            // 跳过 `$$` 分隔符，找到匹配的 `$$`
            let start = i + 2;
            if let Some(end) = text[start..].find("$$") {
                let math = &text[start..start + end];
                out.push_str(&latex_to_unicode(math));
                i = start + end + 2;
                continue;
            }
        }
        // 检测 $ 行内（反斜杠保护 \$ 不算）
        if bytes[i] == b'$' && (i == 0 || bytes[i - 1] != b'\\') {
            // 跳过 $, 找到匹配的 $
            let start = i + 1;
            if let Some(end) = text[start..].find('$') {
                // 过滤货币: "$5" 或 "$10.50" — 数字紧跟 $ 不算数学
                let math = &text[start..start + end];
                if !math.is_empty() && !math.starts_with(|c: char| c.is_ascii_digit()) {
                    out.push_str(&latex_to_unicode(math));
                    i = start + end + 1;
                    continue;
                }
            }
        }
        let ch = text[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
