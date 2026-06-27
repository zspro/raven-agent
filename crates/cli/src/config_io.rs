//! 配置文件读写辅助：按键/按 section 写入 TOML，以及完整序列化保存

/// 截断字符串到24字符
pub(crate) fn truncate_24(s: &str) -> String {
    if s.chars().count() > 24 {
        s.chars().take(21).collect::<String>() + "..."
    } else {
        s.to_string()
    }
}

/// 保存顶层配置项
pub(crate) fn save_config_value(path: &std::path::Path, key: &str, value: &str) {
    let content = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let new_line = format!("{} = \"{}\"", key, value);
    let updated = if let Some(pos) = content.find(&format!("{} = ", key)) {
        let before = &content[..pos];
        let after_start = &content[pos..];
        if let Some(nl) = after_start.find('\n') {
            format!("{}{}\n{}", before, new_line, &content[pos + nl + 1..])
        } else {
            format!("{}{}", before, new_line)
        }
    } else if content.is_empty() {
        format!("# Raven 配置\n{}\n", new_line)
    } else {
        format!("{}\n{}\n", content.trim_end(), new_line)
    };

    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(path, updated);
}

/// 保存 [section] 下的配置项。
///
/// 按行解析，避免手写字节切割的两个陷阱：
/// 1. key 搜索若跨越整个文件，会误改后面 section 中的同名 key；这里把
///    搜索范围限定在「本 section header 行之后、下一个 section header 行之前」。
/// 2. 用 `find('[')` 找下一个 section 会被字符串值 / 数组里的 `[` 干扰；
///    这里只认「去除前导空白后以 `[` 开头」的行作为 section 边界。
pub(crate) fn save_config_section(path: &std::path::Path, section: &str, key: &str, value: &str) {
    let content = if path.exists() {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    let section_header = format!("[{}]", section);
    let new_line = format!("{} = \"{}\"", key, value);
    let key_prefix = format!("{} =", key);
    let key_prefix_sp = format!("{}=", key);

    let is_section_line = |line: &str| line.trim_start().starts_with('[');
    let is_target_key = |line: &str| {
        let t = line.trim_start();
        t.starts_with(&key_prefix) || t.starts_with(&key_prefix_sp)
    };

    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    // 定位本 section 的 header 行
    let header_idx = lines.iter().position(|l| l.trim() == section_header.trim());

    let updated = match header_idx {
        Some(hidx) => {
            // 本 section 范围：header 之后到下一个 section header（或文件末尾）
            let mut end = lines.len();
            for (i, line) in lines.iter().enumerate().skip(hidx + 1) {
                if is_section_line(line) {
                    end = i;
                    break;
                }
            }
            // 在范围内找已存在的 key
            let existing = (hidx + 1..end).find(|&i| is_target_key(&lines[i]));
            match existing {
                Some(i) => {
                    lines[i] = new_line;
                }
                None => {
                    // 插到 section 末尾（end 之前），跳过尾随空行让排版更整齐
                    let mut insert_at = end;
                    while insert_at > hidx + 1 && lines[insert_at - 1].trim().is_empty() {
                        insert_at -= 1;
                    }
                    lines.insert(insert_at, new_line);
                }
            }
            lines.join("\n") + "\n"
        }
        None => {
            // section 不存在，追加到文件末尾
            format!("{}\n{}\n{}\n", content.trim_end(), section_header, new_line)
        }
    };

    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(path, updated);
}

/// 保存完整配置
pub(crate) fn save_full_config(
    path: &std::path::Path,
    cfg: &raven_types::Config,
) -> Result<(), String> {
    let content = toml::to_string_pretty(cfg).map_err(|e| format!("序列化失败: {}", e))?;
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))
        .map_err(|e| format!("创建目录失败: {}", e))?;
    std::fs::write(path, content).map_err(|e| format!("写入失败: {}", e))?;
    Ok(())
}
