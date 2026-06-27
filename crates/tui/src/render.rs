//! Markdown → ANSI 渲染：pulldown-cmark 事件流转义码、syntect 代码高亮、表格绘制、
//! 以及尊重 CJK/ANSI 的显示宽度计算。

use crate::latex::preprocess_chemistry;
use crate::theme::ColorTheme;
use crate::width::{
    char_display_width, spacer, table_border_bottom, table_border_sep, table_border_top,
    truncate_to_width, visible_width,
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

pub(crate) struct Renderer {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Renderer {
    pub(crate) fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    pub(crate) fn render(&self, md: &str) -> String {
        // 预处理：化学方程式 HTML/LaTeX → Unicode 上下标
        let md = preprocess_chemistry(md);

        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        opts.insert(Options::ENABLE_STRIKETHROUGH);
        opts.insert(Options::ENABLE_TASKLISTS);

        let parser = Parser::new_ext(&md, opts);
        let mut out = String::new();
        let mut in_code = false;
        let mut lang = String::new();
        let mut code = String::new();

        // 表格缓冲状态
        let mut in_table = false;
        let mut table_aligns: Vec<pulldown_cmark::Alignment> = Vec::new();
        let mut table_rows: Vec<Vec<String>> = Vec::new();
        let mut table_cur_row: Vec<String> = Vec::new();
        let mut table_cur_cell = String::new();

        // 列表状态：栈结构追踪嵌套列表 (kind, next_index)
        // kind = Some(n) 有序, None 无序
        let mut list_stack: Vec<(Option<u64>, u64)> = Vec::new();

        // 链接状态
        let mut link_url = String::new();

        // 行内样式栈：追踪活跃的样式代码，关闭时恢复外层样式
        let mut style_stack: Vec<&'static str> = Vec::new();

        for ev in parser {
            // ---- 表格缓冲模式 ----
            if in_table {
                match &ev {
                    Event::Start(Tag::TableHead) => continue,
                    Event::End(TagEnd::TableHead) => continue,
                    Event::Start(Tag::TableRow) => {
                        table_cur_row = Vec::new();
                        continue;
                    }
                    Event::End(TagEnd::TableRow) => {
                        if !table_cur_row.is_empty() {
                            table_rows.push(std::mem::take(&mut table_cur_row));
                        }
                        continue;
                    }
                    Event::Start(Tag::TableCell) => {
                        table_cur_cell = String::new();
                        continue;
                    }
                    Event::End(TagEnd::TableCell) => {
                        table_cur_row.push(std::mem::take(&mut table_cur_cell));
                        continue;
                    }
                    Event::End(TagEnd::Table) => {
                        out.push_str(&self.render_table(&table_aligns, &table_rows));
                        out.push('\n');
                        table_aligns.clear();
                        table_rows.clear();
                        in_table = false;
                        continue;
                    }
                    Event::Text(t) => {
                        table_cur_cell.push_str(t);
                        continue;
                    }
                    Event::Code(c) => {
                        table_cur_cell.push_str(&format!(
                            "{}`{c}`{}",
                            ColorTheme::CODE_INLINE,
                            ColorTheme::RESET
                        ));
                        continue;
                    }
                    Event::Start(Tag::Emphasis) => {
                        table_cur_cell.push_str(ColorTheme::ITALIC);
                        continue;
                    }
                    Event::End(TagEnd::Emphasis) => {
                        table_cur_cell.push_str(ColorTheme::RESET);
                        continue;
                    }
                    Event::Start(Tag::Strong) => {
                        table_cur_cell.push_str(ColorTheme::BOLD);
                        continue;
                    }
                    Event::End(TagEnd::Strong) => {
                        table_cur_cell.push_str(ColorTheme::RESET);
                        continue;
                    }
                    _ => continue,
                }
            }

            match ev {
                // ---- 代码块 ----
                Event::Start(Tag::CodeBlock(kind)) => {
                    in_code = true;
                    lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                        _ => String::new(),
                    };
                }
                Event::End(TagEnd::CodeBlock) => {
                    out.push_str(&self.code_block(&lang, &code));
                    code.clear();
                    in_code = false;
                }
                Event::Text(t) if in_code => code.push_str(&t),

                // ---- 表格入口 ----
                Event::Start(Tag::Table(aligns)) => {
                    in_table = true;
                    table_aligns = aligns;
                    table_rows = Vec::new();
                    table_cur_row = Vec::new();
                    table_cur_cell = String::new();
                }

                // ---- 普通文本 ----
                Event::Text(t) => out.push_str(&t),

                // ---- 标题 ----
                Event::Start(Tag::Heading { level, .. }) => {
                    out.push_str(match level {
                        pulldown_cmark::HeadingLevel::H1 => ColorTheme::HEADING_H1,
                        pulldown_cmark::HeadingLevel::H2 => ColorTheme::HEADING_H2,
                        pulldown_cmark::HeadingLevel::H3 => ColorTheme::HEADING_H3,
                        _ => ColorTheme::BOLD,
                    });
                }
                Event::End(TagEnd::Heading(_)) => {
                    out.push_str(ColorTheme::RESET);
                    out.push('\n');
                }

                // ---- 行内样式（样式栈确保嵌套不丢失）----
                Event::Start(Tag::Emphasis) => {
                    style_stack.push(ColorTheme::ITALIC);
                    out.push_str(ColorTheme::ITALIC);
                }
                Event::End(TagEnd::Emphasis) => {
                    style_stack.retain(|&s| s != ColorTheme::ITALIC);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack {
                        out.push_str(s);
                    }
                }
                Event::Start(Tag::Strong) => {
                    style_stack.push(ColorTheme::BOLD);
                    out.push_str(ColorTheme::BOLD);
                }
                Event::End(TagEnd::Strong) => {
                    style_stack.retain(|&s| s != ColorTheme::BOLD);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack {
                        out.push_str(s);
                    }
                }
                Event::Code(c) => out.push_str(&format!(
                    "{}`{c}`{}",
                    ColorTheme::CODE_INLINE,
                    ColorTheme::RESET
                )),
                Event::Start(Tag::Strikethrough) => {
                    style_stack.push(ColorTheme::STRIKETHROUGH);
                    out.push_str(ColorTheme::STRIKETHROUGH);
                }
                Event::End(TagEnd::Strikethrough) => {
                    style_stack.retain(|&s| s != ColorTheme::STRIKETHROUGH);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack {
                        out.push_str(s);
                    }
                }

                // ---- 引用 ----
                Event::Start(Tag::BlockQuote(_)) => out.push_str(ColorTheme::QUOTE),
                Event::End(TagEnd::BlockQuote(_)) => out.push_str(ColorTheme::RESET),

                // ---- 链接（样式栈保留外层样式）----
                Event::Start(Tag::Link { dest_url, .. }) => {
                    link_url = dest_url.to_string();
                    style_stack.push(ColorTheme::LINK);
                    style_stack.push(ColorTheme::UNDERLINE);
                    out.push_str(ColorTheme::LINK);
                    out.push_str(ColorTheme::UNDERLINE);
                }
                Event::End(TagEnd::Link) => {
                    style_stack.retain(|&s| s != ColorTheme::LINK && s != ColorTheme::UNDERLINE);
                    out.push_str(ColorTheme::RESET);
                    for &s in &style_stack {
                        out.push_str(s);
                    }
                    // 链接后附 URL
                    out.push_str(&format!(
                        "{} ({}){}",
                        ColorTheme::DIM,
                        link_url,
                        ColorTheme::RESET
                    ));
                    link_url.clear();
                }

                // ---- 列表（栈追踪嵌套）----
                Event::Start(Tag::List(number)) => {
                    let start = number.unwrap_or(1);
                    list_stack.push((number, start));
                    // 嵌套缩进：每层 2 空格
                    let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                    out.push_str(&indent);
                }
                Event::End(TagEnd::List(_)) => {
                    list_stack.pop();
                    // 最外层列表结束时加空行
                    if list_stack.is_empty() {
                        out.push('\n');
                    }
                }
                Event::Start(Tag::Item) => {
                    let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                    if let Some((kind, idx)) = list_stack.last_mut() {
                        match kind {
                            Some(_) => {
                                // 有序列表：自动编号
                                out.push_str(&format!(
                                    "{indent}{bold}{accent}{idx}.{rst} ",
                                    bold = ColorTheme::BOLD,
                                    accent = ColorTheme::ACCENT,
                                    rst = ColorTheme::RESET,
                                ));
                                *idx += 1;
                            }
                            None => {
                                // 无序列表
                                out.push_str(&format!(
                                    "{indent}{bullet}·{rst} ",
                                    bullet = ColorTheme::BULLET,
                                    rst = ColorTheme::RESET,
                                ));
                            }
                        }
                    }
                }
                Event::End(TagEnd::Item) => out.push('\n'),

                // ---- 任务列表 ----
                Event::TaskListMarker(checked) => {
                    if checked {
                        out.push_str(&format!("{}[x]{} ", ColorTheme::SUCCESS, ColorTheme::RESET));
                    } else {
                        out.push_str(&format!("{}[ ]{} ", ColorTheme::DIM, ColorTheme::RESET));
                    }
                }

                // ---- 段落 / 换行 ----
                Event::End(TagEnd::Paragraph) => out.push('\n'),
                Event::HardBreak => out.push('\n'),
                Event::SoftBreak => out.push(' '),

                _ => {}
            }
        }
        out
    }

    /// 渲染表格：计算列宽 + 绘制 Unicode 制表符边框
    fn render_table(&self, _aligns: &[pulldown_cmark::Alignment], rows: &[Vec<String>]) -> String {
        if rows.is_empty() {
            return String::new();
        }

        let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(1);
        // 计算每列的显示宽度（去除 ANSI 转义码）
        let mut col_widths = vec![3usize; ncols]; // 最小 3
        for row in rows {
            for (ci, cell) in row.iter().enumerate() {
                let w = visible_width(cell).max(3);
                if w > col_widths[ci] {
                    col_widths[ci] = w.min(50); // 单列最大 50
                }
            }
        }

        let mut out = String::new();
        out.push('\n');

        let is_header = !rows.is_empty();

        for (ri, row) in rows.iter().enumerate() {
            let is_hdr = ri == 0 && is_header;
            // 顶部边框（仅表头行前）
            if ri == 0 {
                out.push_str(&table_border_top(&col_widths));
            }
            // 行内容
            for (ci, w) in col_widths.iter().enumerate() {
                let raw = row.get(ci).map(|s| s.as_str()).unwrap_or("");
                // 超过列宽的单元格按可见宽度截断（补 …），保证右边框与顶部边框对齐
                let cell = truncate_to_width(raw, *w);
                let pad = visible_width(&cell);
                let extra = if pad < *w { w - pad } else { 0 };
                let pad_str = spacer(extra);
                if is_hdr {
                    out.push_str(&format!(
                        "{dim}│{rst} {bold}{cell}{rst}{pad_str} ",
                        dim = ColorTheme::DIM,
                        rst = ColorTheme::RESET,
                        bold = ColorTheme::BOLD,
                    ));
                } else {
                    out.push_str(&format!(
                        "{dim}│{rst} {cell}{pad_str} ",
                        dim = ColorTheme::DIM,
                        rst = ColorTheme::RESET,
                    ));
                }
            }
            out.push_str(&format!(
                "{dim}│{rst}\n",
                dim = ColorTheme::DIM,
                rst = ColorTheme::RESET
            ));

            // 表头分隔线
            if is_hdr {
                out.push_str(&table_border_sep(&col_widths));
            }
        }
        // 底部边框
        out.push_str(&table_border_bottom(&col_widths));
        out
    }

    fn code_block(&self, lang: &str, code: &str) -> String {
        let syntax = if lang.is_empty() {
            self.syntax_set.find_syntax_plain_text()
        } else {
            self.syntax_set
                .find_syntax_by_token(lang)
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
        };
        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);
        // 框宽取「所有代码行的最大显示宽度」，且按显示宽度（CJK 记 2）而非
        // 字节长度计算——否则含中文的代码行会让边框过宽且对不齐。
        let w = code
            .lines()
            .map(|l| l.chars().map(char_display_width).sum::<usize>())
            .max()
            .unwrap_or(0)
            .max(40);

        let dim = ColorTheme::DIM;
        let rst = ColorTheme::RESET;
        let bg = ColorTheme::CODE_BG;

        let mut out = String::new();
        out.push_str(&format!("{dim}╭{}╮{rst}\n", "─".repeat(w + 2)));

        for line in code.lines() {
            match h.highlight_line(line, &self.syntax_set) {
                Ok(ranges) => {
                    let styled: String = ranges
                        .iter()
                        .map(|(s, t)| {
                            format!(
                                "\x1b[38;2;{};{};{}m{bg}{t}",
                                s.foreground.r, s.foreground.g, s.foreground.b
                            )
                        })
                        .collect();
                    out.push_str(&format!("{dim}│{rst} {styled}{rst}\n"));
                }
                Err(_) => out.push_str(&format!("{dim}│{rst} {bg}{line}{rst}\n")),
            }
        }

        out.push_str(&format!("{dim}╰{}╯{rst}\n", "─".repeat(w + 2)));
        out
    }
}
