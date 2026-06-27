//! LaTeX 数学 → Unicode 上下标 / 符号转换：把 `latex.rs` 预处理出的数学片段
//! 转成纯终端可读的 Unicode（希腊字母、箭头、运算符、上下标、分数等）。

/// 把一段 LaTeX 数学转成 Unicode（递归处理上下标、命令、花括号）。
pub(crate) fn latex_to_unicode(math: &str) -> String {
    let mut out = String::with_capacity(math.len());
    let chars: Vec<char> = math.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            // 下标: _ 后跟单字符 或 _{...}（不可映射时 fallback _(...)）
            '_' => {
                i += 1;
                if i < chars.len() && chars[i] == '{' {
                    i += 1;
                    let content = extract_brace_group(&chars, &mut i);
                    let inner = latex_to_unicode(&content);
                    if all_subscriptable(&inner) {
                        out.push_str(&sub_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("_({inner})"));
                    }
                    i += 1;
                } else if i < chars.len() {
                    let ch = chars[i];
                    let inner = latex_to_unicode(&ch.to_string());
                    if all_subscriptable(&inner) {
                        out.push_str(&sub_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("_({inner})"));
                    }
                    i += 1;
                }
            }
            // 上标: ^ 后跟单字符 或 ^{...}（不可映射时 fallback ^(...)）
            '^' => {
                i += 1;
                if i < chars.len() && chars[i] == '{' {
                    i += 1;
                    let content = extract_brace_group(&chars, &mut i);
                    let inner = latex_to_unicode(&content);
                    if all_superscriptable(&inner) {
                        out.push_str(&sup_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("^({inner})"));
                    }
                    i += 1;
                } else if i < chars.len() {
                    let ch = chars[i];
                    let inner = latex_to_unicode(&ch.to_string());
                    if all_superscriptable(&inner) {
                        out.push_str(&sup_to_unicode(&inner));
                    } else {
                        out.push_str(&format!("^({inner})"));
                    }
                    i += 1;
                }
            }
            // 反斜杠命令
            '\\' => {
                i += 1;
                let cmd_start = i;
                while i < chars.len() && chars[i].is_alphabetic() {
                    i += 1;
                }
                let cmd: String = chars[cmd_start..i].iter().collect();

                // 空命令（\; \, \: \! \ 等）→ 空格
                if cmd.is_empty() {
                    out.push(' ');
                    // 跳过空格类字符: 空格本身 \; \, \: \!  → 都已消费
                    if i < chars.len() && matches!(chars[i], ';' | ',' | ':' | '!') {
                        i += 1;
                    }
                    continue;
                }

                latex_command_to_unicode(&cmd, &chars, &mut i, &mut out);
            }
            // 花括号 — 数学模式中裸花括号跳过（已由分数/文本等处理）
            '{' | '}' => {
                i += 1;
            }
            // 其他字符
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }

    out
}
// PLACEHOLDER_CMD
/// 处理单个 LaTeX 反斜杠命令（已剥离反斜杠），把结果写入 `out`，并按需推进 `i`。
fn latex_command_to_unicode(cmd: &str, chars: &[char], i: &mut usize, out: &mut String) {
    match cmd {
        // 箭头
        "rightarrow" | "to" | "longrightarrow" => out.push('\u{2192}'),
        "leftarrow" | "longleftarrow" => out.push('\u{2190}'),
        "leftrightarrow" | "longleftrightarrow" => out.push('\u{2194}'),
        "Rightarrow" | "Longrightarrow" => out.push('\u{21D2}'),
        "Leftarrow" | "Longleftarrow" => out.push('\u{21D0}'),
        "uparrow" => out.push('\u{2191}'),
        "downarrow" => out.push('\u{2193}'),
        "rightleftharpoons" => out.push('\u{21CC}'),
        // 带文字箭头: \xrightarrow{text} → →(text)
        "xrightarrow" | "xleftarrow" | "xleftrightarrow" => {
            let arrow = match cmd {
                "xrightarrow" => '\u{2192}',
                "xleftarrow" => '\u{2190}',
                _ => '\u{2194}',
            };
            if *i < chars.len() && chars[*i] == '{' {
                *i += 1;
                let text = extract_brace_group(chars, i);
                out.push_str(&format!("\u{002D}{text}\u{2192}"));
                *i += 1;
            } else {
                out.push(arrow);
            }
        }
        // 希腊字母
        "Delta" => out.push('\u{0394}'),
        "Gamma" => out.push('\u{0393}'),
        "alpha" => out.push('\u{03B1}'),
        "beta" => out.push('\u{03B2}'),
        "gamma" => out.push('\u{03B3}'),
        "delta" => out.push('\u{03B4}'),
        "epsilon" | "varepsilon" => out.push('\u{03B5}'),
        "zeta" => out.push('\u{03B6}'),
        "eta" => out.push('\u{03B7}'),
        "theta" => out.push('\u{03B8}'),
        "lambda" => out.push('\u{03BB}'),
        "mu" => out.push('\u{03BC}'),
        "nu" => out.push('\u{03BD}'),
        "xi" => out.push('\u{03BE}'),
        "pi" => out.push('\u{03C0}'),
        "rho" => out.push('\u{03C1}'),
        "sigma" => out.push('\u{03C3}'),
        "tau" => out.push('\u{03C4}'),
        "phi" | "varphi" => out.push('\u{03C6}'),
        "omega" => out.push('\u{03C9}'),
        // 运算符
        "times" => out.push('\u{00D7}'),
        "cdot" => out.push('\u{22C5}'),
        "pm" => out.push('\u{00B1}'),
        "mp" => out.push('\u{2213}'),
        "div" => out.push('\u{00F7}'),
        "infty" => out.push('\u{221E}'),
        "approx" => out.push('\u{2248}'),
        "equiv" => out.push('\u{2261}'),
        "neq" | "ne" => out.push('\u{2260}'),
        "leq" | "le" => out.push('\u{2264}'),
        "geq" | "ge" => out.push('\u{2265}'),
        "ll" => out.push('\u{226A}'),
        "gg" => out.push('\u{226B}'),
        "sim" => out.push('\u{223C}'),
        "propto" => out.push('\u{221D}'),
        "partial" => out.push('\u{2202}'),
        "nabla" => out.push('\u{2207}'),
        "sum" => out.push('\u{2211}'),
        "prod" => out.push('\u{220F}'),
        "int" => out.push('\u{222B}'),
        "oint" => out.push('\u{222E}'),
        "sqrt" => out.push('\u{221A}'),
        "degree" | "circ" => out.push('\u{00B0}'),
        // 文本模式: \mathrm, \text, \mathbf → 纯文本
        "mathrm" | "text" | "mathbf" | "mathcal" | "mathit" | "mathsf" | "mathtt" => {
            if *i < chars.len() && chars[*i] == '{' {
                *i += 1;
                let content = extract_brace_group(chars, i);
                out.push_str(&latex_to_unicode(&content));
                *i += 1;
            }
        }
        // 分数: \frac{a}{b} → (a)/(b)  或  用 Unicode 分割线
        "frac" => {
            if *i < chars.len() && chars[*i] == '{' {
                *i += 1;
                let num = extract_brace_group(chars, i);
                *i += 1; // skip }
                if *i < chars.len() && chars[*i] == '{' {
                    *i += 1;
                    let den = extract_brace_group(chars, i);
                    out.push_str(&format!(
                        "({})/({})",
                        latex_to_unicode(&num),
                        latex_to_unicode(&den)
                    ));
                    *i += 1;
                } else {
                    out.push_str(&latex_to_unicode(&num));
                }
            }
        }
        // 极限: \lim → lim
        "lim" => out.push_str("lim"),
        // 空格类
        "quad" | "qquad" => out.push(' '),
        // 无操作（忽略）
        "displaystyle" | "textstyle" | "scriptstyle" => {}
        // \boxed{...} → 提取内容（数学模式内）
        "boxed" => {
            if *i < chars.len() && chars[*i] == '{' {
                *i += 1;
                let content = extract_brace_group(chars, i);
                out.push_str(&latex_to_unicode(&content));
                *i += 1;
            }
        }
        // 换行: \\ → 换行（只在数学模式内部，align 等）
        // 未知命令 → 去掉反斜杠，保留命令名
        _ => {
            out.push_str(cmd);
        }
    }
}
/// 从 chars[i..] 提取花括号组内容，i 前进到对应的 '}'
fn extract_brace_group(chars: &[char], i: &mut usize) -> String {
    let start = *i;
    let mut depth = 1usize;
    while *i < chars.len() && depth > 0 {
        if chars[*i] == '{' {
            depth += 1;
        }
        if chars[*i] == '}' {
            depth -= 1;
        }
        if depth > 0 {
            *i += 1;
        }
    }
    chars[start..*i].iter().collect()
}
pub(crate) fn sub_to_unicode(s: &str) -> String {
    s.chars().map(sub_to_unicode_char).collect()
}

pub(crate) fn sup_to_unicode(s: &str) -> String {
    s.chars().map(sup_to_unicode_char).collect()
}

/// 字符串中所有字符是否均可转为下标/上标
pub(crate) fn all_subscriptable(s: &str) -> bool {
    s.chars().all(|c| sub_to_unicode_char(c) != c || c == ' ')
}
pub(crate) fn all_superscriptable(s: &str) -> bool {
    s.chars().all(|c| sup_to_unicode_char(c) != c || c == ' ')
}

fn sub_to_unicode_char(c: char) -> char {
    match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        '+' => '₊',
        '-' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        _ => c,
    }
}

fn sup_to_unicode_char(c: char) -> char {
    match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        '+' => '⁺',
        '-' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        _ => c,
    }
}
