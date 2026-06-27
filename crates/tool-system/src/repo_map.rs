//! Repo Map - 借鉴 Aider
//!
//! 扫描项目目录，生成代码地图（文件列表、关键符号、依赖关系），
//! 帮助 Agent 快速了解大型项目结构。
//!
//! Aider 使用 Tree-sitter 做语法分析，我们这里用轻量级方法：
//! 基于文件扩展名和简单的正则匹配提取关键符号。

use std::path::{Path, PathBuf};

/// 文件类型
#[derive(Debug, Clone)]
pub enum FileKind {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    Cpp,
    Markdown,
    Config,
    Other(String),
}

impl FileKind {
    fn from_ext(ext: &str) -> Self {
        match ext {
            "rs" => FileKind::Rust,
            "py" => FileKind::Python,
            "js" => FileKind::JavaScript,
            "ts" | "tsx" => FileKind::TypeScript,
            "go" => FileKind::Go,
            "java" => FileKind::Java,
            "cpp" | "cc" | "hpp" | "h" => FileKind::Cpp,
            "md" | "mdx" => FileKind::Markdown,
            "toml" | "yaml" | "yml" | "json" | "conf" | "ini" => FileKind::Config,
            other => FileKind::Other(other.to_string()),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            FileKind::Rust => "🦀",
            FileKind::Python => "🐍",
            FileKind::JavaScript => "🟨",
            FileKind::TypeScript => "📘",
            FileKind::Go => "🔵",
            FileKind::Java => "☕",
            FileKind::Cpp => "🔷",
            FileKind::Markdown => "📝",
            FileKind::Config => "⚙️",
            FileKind::Other(_) => "📄",
        }
    }
}

/// 代码符号
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub enum SymbolKind {
    Function,
    Struct,
    Trait,
    Enum,
    Module,
    Const,
    Type,
    Class,
    Method,
}

impl SymbolKind {
    fn icon(&self) -> &'static str {
        match self {
            SymbolKind::Function => "ƒ",
            SymbolKind::Struct => "§",
            SymbolKind::Trait => "◊",
            SymbolKind::Enum => "Σ",
            SymbolKind::Module => "▣",
            SymbolKind::Const => "π",
            SymbolKind::Type => "τ",
            SymbolKind::Class => "©",
            SymbolKind::Method => "µ",
        }
    }
}

/// Repo Map 条目
#[derive(Debug, Clone)]
pub struct RepoMapEntry {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub symbols: Vec<Symbol>,
}

/// Repo Map 生成器
pub struct RepoMap;

impl RepoMap {
    /// 生成项目代码地图
    pub fn generate(project_path: impl AsRef<Path>, max_depth: usize) -> Vec<RepoMapEntry> {
        let path = project_path.as_ref();
        let mut entries = Vec::new();
        Self::scan_dir(path, path, 0, max_depth, &mut entries);
        entries
    }

    /// 格式化代码地图为文本
    pub fn format(entries: &[RepoMapEntry]) -> String {
        let mut lines = Vec::new();

        // 统计
        let total_files = entries.len();
        let total_size: u64 = entries.iter().map(|e| e.size).sum();
        let total_symbols: usize = entries.iter().map(|e| e.symbols.len()).sum();

        lines.push(format!(
            "📊 Repo Map: {} 文件, {} 符号, {} 总计",
            total_files,
            total_symbols,
            format_size(total_size)
        ));
        lines.push("═".repeat(60));

        // 显示每个文件
        for entry in entries {
            let icon = entry.kind.icon();
            let size = format_size(entry.size);
            let path_str = entry.path.display().to_string();

            lines.push(format!("{} {} ({})", icon, path_str, size));

            // 显示关键符号（最多5个）
            for sym in entry.symbols.iter().take(5) {
                lines.push(format!(
                    "   {} {} (第{}行)",
                    sym.kind.icon(),
                    sym.name,
                    sym.line
                ));
            }
            if entry.symbols.len() > 5 {
                lines.push(format!("   ... 还有 {} 个符号", entry.symbols.len() - 5));
            }
        }

        lines.join("\n")
    }

    /// 生成精简版（只显示文件列表和数量）
    pub fn format_compact(entries: &[RepoMapEntry]) -> String {
        let mut lines = Vec::new();
        let total_files = entries.len();
        let total_symbols: usize = entries.iter().map(|e| e.symbols.len()).sum();

        lines.push(format!(
            "Repo Map: {} 文件, {} 符号",
            total_files, total_symbols
        ));
        lines.push("─".repeat(40));

        for e in entries {
            let sym_count = e.symbols.len();
            let sym_str = if sym_count > 0 {
                format!(" ({} 符号)", sym_count)
            } else {
                String::new()
            };
            lines.push(format!("{} {}{}", e.kind.icon(), e.path.display(), sym_str));
        }

        lines.join("\n")
    }

    // ===================================================================
    // 内部方法
    // ===================================================================

    fn scan_dir(
        root: &Path,
        current: &Path,
        depth: usize,
        max_depth: usize,
        entries: &mut Vec<RepoMapEntry>,
    ) {
        if depth > max_depth {
            return;
        }

        let Ok(reader) = std::fs::read_dir(current) else {
            return;
        };

        for entry in reader.filter_map(|e| e.ok()) {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            // 跳过隐藏目录和常见忽略目录
            if name.starts_with('.') && name != ".github" && name != ".cargo" {
                continue;
            }
            if ["target", "node_modules", "vendor", "dist", "build"].contains(&name.as_ref()) {
                continue;
            }

            if path.is_dir() {
                Self::scan_dir(root, &path, depth + 1, max_depth, entries);
            } else if path.is_file() {
                if let Some(entry) = Self::analyze_file(root, &path) {
                    entries.push(entry);
                }
            }
        }
    }

    fn analyze_file(root: &Path, path: &Path) -> Option<RepoMapEntry> {
        let ext = path.extension()?.to_string_lossy();
        let kind = FileKind::from_ext(&ext);

        // 只分析代码文件
        let is_code = matches!(
            kind,
            FileKind::Rust
                | FileKind::Python
                | FileKind::JavaScript
                | FileKind::TypeScript
                | FileKind::Go
                | FileKind::Java
                | FileKind::Cpp
        );

        if !is_code {
            return None;
        }

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        // 只分析小于 100KB 的文件
        if size > 100_000 {
            let rel_path = path.strip_prefix(root).unwrap_or(path);
            return Some(RepoMapEntry {
                path: rel_path.to_path_buf(),
                kind,
                size,
                symbols: vec![], // 太大不分析
            });
        }

        let content = std::fs::read_to_string(path).unwrap_or_default();
        let symbols = Self::extract_symbols(&kind, &content);

        let rel_path = path.strip_prefix(root).unwrap_or(path);
        Some(RepoMapEntry {
            path: rel_path.to_path_buf(),
            kind,
            size,
            symbols,
        })
    }

    /// 从代码中提取关键符号
    fn extract_symbols(kind: &FileKind, content: &str) -> Vec<Symbol> {
        match kind {
            FileKind::Rust => Self::extract_rust_symbols(content),
            FileKind::Python => Self::extract_python_symbols(content),
            FileKind::Go => Self::extract_go_symbols(content),
            FileKind::JavaScript | FileKind::TypeScript => Self::extract_js_symbols(content),
            _ => Vec::new(),
        }
    }

    fn extract_rust_symbols(content: &str) -> Vec<Symbol> {
        let patterns = [
            (r"^\s*(?:pub\s+)?fn\s+(\w+)", SymbolKind::Function),
            (r"^\s*(?:pub\s+)?struct\s+(\w+)", SymbolKind::Struct),
            (r"^\s*(?:pub\s+)?trait\s+(\w+)", SymbolKind::Trait),
            (r"^\s*(?:pub\s+)?enum\s+(\w+)", SymbolKind::Enum),
            (r"^\s*(?:pub\s+)?mod\s+(\w+)", SymbolKind::Module),
            (r"^\s*(?:pub\s+)?const\s+(\w+)", SymbolKind::Const),
            (r"^\s*(?:pub\s+)?type\s+(\w+)", SymbolKind::Type),
        ];
        Self::extract_by_patterns(content, &patterns)
    }

    fn extract_python_symbols(content: &str) -> Vec<Symbol> {
        let patterns = [
            (r"^\s*def\s+(\w+)", SymbolKind::Function),
            (r"^\s*class\s+(\w+)", SymbolKind::Class),
        ];
        Self::extract_by_patterns(content, &patterns)
    }

    fn extract_go_symbols(content: &str) -> Vec<Symbol> {
        let patterns = [
            (r"^\s*func\s+(?:\(.*\)\s+)?(\w+)", SymbolKind::Function),
            (
                r"^\s*type\s+(\w+)\s+(?:struct|interface)",
                SymbolKind::Struct,
            ),
        ];
        Self::extract_by_patterns(content, &patterns)
    }

    fn extract_js_symbols(content: &str) -> Vec<Symbol> {
        let patterns = [
            (
                r"^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)",
                SymbolKind::Function,
            ),
            (r"^\s*(?:export\s+)?class\s+(\w+)", SymbolKind::Class),
            (r"^\s*const\s+(\w+)\s*=[^=]", SymbolKind::Const),
        ];
        Self::extract_by_patterns(content, &patterns)
    }

    /// 用一组（正则, 符号类型）扫描内容提取符号。
    ///
    /// 正则在按行扫描前一次性编译，避免在「每行 × 每模式」内重复编译
    /// （这是之前的性能 bug：大文件会触发成千上万次正则编译）。
    fn extract_by_patterns(content: &str, patterns: &[(&str, SymbolKind)]) -> Vec<Symbol> {
        let compiled: Vec<(regex::Regex, &SymbolKind)> = patterns
            .iter()
            .filter_map(|(pat, kind)| regex::Regex::new(pat).ok().map(|re| (re, kind)))
            .collect();

        let mut symbols = Vec::new();
        for (i, line) in content.lines().enumerate() {
            for (re, kind) in &compiled {
                if let Some(cap) = re.captures(line) {
                    if let Some(name) = cap.get(1) {
                        symbols.push(Symbol {
                            name: name.as_str().to_string(),
                            kind: (*kind).clone(),
                            line: i + 1,
                        });
                    }
                }
            }
        }
        symbols
    }
}

/// 格式化文件大小
fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}", size, UNITS[unit_idx])
}
