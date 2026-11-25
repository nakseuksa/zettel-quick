use crate::config::AppConfig;
use crate::draft::{DraftSession, LastArchivedCard};
use chrono::{Local, SecondsFormat};
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArchiveOutcome {
    EmptySkipped,
    Saved {
        file_path: String,
        filename: String,
        updated_existing: bool,
    },
    DeletedRecalled {
        deleted_path: String,
    },
}

/// 判断是否为 Windows 保留设备名
fn is_windows_reserved_name(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// 若文本以用户手动输入的 YAML Frontmatter 开头，返回正文起始字节偏移
pub fn split_leading_frontmatter(text: &str) -> Option<(&str, &str)> {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let after_open = normalized
        .strip_prefix("---\r\n")
        .or_else(|| normalized.strip_prefix("---\n"))?;

    let mut pos = 0usize;
    for line in after_open.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" || trimmed == "..." {
            let fm_end = pos;
            let body_start = pos + line.len();
            return Some((&after_open[..fm_end], &after_open[body_start..]));
        }
        pos += line.len();
    }
    None
}

/// 剥离行内图片引用 ![[...]] 与 ![alt](url)，并将 [[target|alias]] 转为可读文本
fn clean_markdown_links_and_embeds(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        // 剥离 ![[...]]
        if i + 2 < n && chars[i] == '!' && chars[i + 1] == '[' && chars[i + 2] == '[' {
            i += 3;
            while i + 1 < n && !(chars[i] == ']' && chars[i + 1] == ']') {
                i += 1;
            }
            if i + 1 < n {
                i += 2;
            }
            continue;
        }

        // 剥离 ![alt](url)
        if i + 1 < n && chars[i] == '!' && chars[i + 1] == '[' {
            if let Some(close_bracket) = chars[i + 2..].iter().position(|&c| c == ']') {
                let after_bracket = i + 2 + close_bracket + 1;
                if after_bracket < n && chars[after_bracket] == '(' {
                    if let Some(close_paren) = chars[after_bracket + 1..].iter().position(|&c| c == ')') {
                        i = after_bracket + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        // 展开 [[target|alias]] -> alias 或 [[target]] -> target
        if i + 1 < n && chars[i] == '[' && chars[i + 1] == '[' {
            let start = i + 2;
            let mut end = None;
            let mut j = start;
            while j + 1 < n {
                if chars[j] == ']' && chars[j + 1] == ']' {
                    end = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(e) = end {
                let inner: String = chars[start..e].iter().collect();
                let display = inner
                    .split_once('|')
                    .map(|(_, alias)| alias)
                    .unwrap_or(inner.as_str());
                out.push_str(display);
                i = e + 2;
                continue;
            }
        }

        // 展开普通链接 [text](url) -> text
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let text_end = i + 1 + close_bracket;
                if text_end + 1 < n && chars[text_end + 1] == '(' {
                    if let Some(close_paren) = chars[text_end + 2..].iter().position(|&c| c == ')') {
                        let label: String = chars[i + 1..text_end].iter().collect();
                        out.push_str(&label);
                        i = text_end + 2 + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 剥离行内的 #标签 词元，避免文件名与标题摘要混入标签符号
fn strip_inline_tags_from_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let prev_ok = i == 0 || chars[i - 1].is_whitespace();
        if prev_ok && chars[i] == '#' {
            let mut j = i + 1;
            let mut has_non_digit = false;
            while j < n && is_valid_tag_char(chars[j]) {
                if !chars[j].is_ascii_digit() {
                    has_non_digit = true;
                }
                j += 1;
            }
            if j > i + 1 && has_non_digit {
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub fn is_valid_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// 从卡片正文提取首行智能摘要（清洗 Markdown 标记、双链外壳、内联标签与 Windows 非法文件名字符）
pub fn extract_slug(content: &str, max_chars: usize) -> String {
    let body = split_leading_frontmatter(content)
        .map(|(_, b)| b)
        .unwrap_or(content);

    let mut in_fence = false;
    for raw_line in body.lines() {
        let trimmed_start = raw_line.trim_start();
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed_start.is_empty() {
            continue;
        }

        // 剥离行首标题、引用、任务列表与无序/有序列表前缀
        let mut s = trimmed_start;
        if s.starts_with('#') {
            let hashes = s.chars().take_while(|&c| c == '#').count();
            if hashes <= 6 {
                let rest = &s[hashes..];
                if rest.starts_with(char::is_whitespace) {
                    s = rest.trim_start();
                }
            }
        }
        while let Some(rest) = s.strip_prefix('>') {
            s = rest.trim_start();
        }
        for prefix in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "- ", "* ", "+ "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.trim_start();
                break;
            }
        }
        // 剥离数字列表如 "1. "
        let digit_bytes = s.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_bytes > 0 && s[digit_bytes..].starts_with(". ") {
            s = s[digit_bytes + 2..].trim_start();
        }

        let cleaned = clean_markdown_links_and_embeds(s);
        let without_tags = strip_inline_tags_from_line(&cleaned);

        // 剔除行内格式符与 Windows 非法路径字符
        let mut sanitized = String::with_capacity(without_tags.len());
        for ch in without_tags.chars() {
            if ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                sanitized.push(' ');
            } else if matches!(ch, '`' | '~') {
                continue;
            } else {
                sanitized.push(ch);
            }
        }

        // 剔除成对或零散的粗体/斜体星号，合并连续空白
        let sanitized = sanitized.replace("**", "").replace("__", "");
        let collapsed: String = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = collapsed.trim_matches(|c: char| c.is_whitespace() || c == '.');

        if trimmed.is_empty() {
            continue;
        }

        let truncated: String = trimmed.chars().take(max_chars.max(1)).collect();
        let final_slug = truncated
            .trim_end_matches(|c: char| c.is_whitespace() || c == '.' || c == '-')
            .to_string();

        if final_slug.is_empty() {
            continue;
        }
        if is_windows_reserved_name(&final_slug) {
            return format!("_{final_slug}");
        }
        return final_slug;
    }

    String::new()
}

/// 提取正文中的所有合法 Obsidian 内联 #标签（自动忽略代码块、行内代码、URL 锚点与双链内部）
pub fn extract_inline_tags(content: &str) -> Vec<String> {
    let body = split_leading_frontmatter(content)
        .map(|(_, b)| b)
        .unwrap_or(content);

    let mut seen = HashSet::new();
    let mut ordered = Vec::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }

        let chars: Vec<char> = line.chars().collect();
        let n = chars.len();
        let mut i = 0;
        let mut in_inline_code = false;

        while i < n {
            if chars[i] == '`' {
                in_inline_code = !in_inline_code;
                i += 1;
                continue;
            }
            if in_inline_code {
                i += 1;
                continue;
            }
            // 跳过 [[...]] 双链与嵌入内部（避免将 [[Note#Heading]] 误判为标签）
            if i + 1 < n && chars[i] == '[' && chars[i + 1] == '[' {
                i += 2;
                while i + 1 < n && !(chars[i] == ']' && chars[i + 1] == ']') {
                    i += 1;
                }
                i += 2;
                continue;
            }

            let prev_ok = i == 0 || chars[i - 1].is_whitespace();
            if prev_ok && chars[i] == '#' {
                let mut j = i + 1;
                let mut has_non_digit = false;
                while j < n && is_valid_tag_char(chars[j]) {
                    if !chars[j].is_ascii_digit() {
                        has_non_digit = true;
                    }
                    j += 1;
                }
                if j > i + 1 && has_non_digit {
                    let raw_tag: String = chars[i + 1..j].iter().collect();
                    let tag = raw_tag.trim_matches('/').to_string();
                    if !tag.is_empty() && seen.insert(tag.clone()) {
                        ordered.push(tag);
                    }
                    i = j;
                    continue;
                }
            }
            i += 1;
        }
    }

    ordered
}

fn yaml_quote(val: &str) -> String {
    let escaped = val.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// 组装完整的 Markdown 落盘内容（含 YAML Frontmatter）
pub fn build_markdown_document(
    cfg: &AppConfig,
    id: &str,
    created_iso: &str,
    slug: &str,
    body: &str,
) -> String {
    let normalized_body = body.replace("\r\n", "\n");
    let mut final_body = normalized_body.trim_end_matches('\n').to_string();
    final_body.push('\n');

    // 若用户已在正文开头手动编写了 YAML Frontmatter，或配置关闭了 Frontmatter，直接保留原样
    if !cfg.frontmatter.enabled || split_leading_frontmatter(&final_body).is_some() {
        return final_body;
    }

    let mut fm_lines = Vec::new();
    fm_lines.push("---".to_string());

    if cfg.frontmatter.include_id {
        fm_lines.push(format!("id: {}", yaml_quote(id)));
    }
    if cfg.frontmatter.include_created {
        fm_lines.push(format!("created: {created_iso}"));
    }

    let mut merged_tags: Vec<String> = Vec::new();
    let mut tag_seen = HashSet::new();
    for extra in &cfg.frontmatter.extra_tags {
        let clean = extra.trim().trim_start_matches('#').to_string();
        if !clean.is_empty() && tag_seen.insert(clean.clone()) {
            merged_tags.push(clean);
        }
    }
    if cfg.frontmatter.sync_inline_tags {
        for inline in extract_inline_tags(&final_body) {
            if tag_seen.insert(inline.clone()) {
                merged_tags.push(inline);
            }
        }
    }

    if !merged_tags.is_empty() {
        fm_lines.push("tags:".to_string());
        for t in &merged_tags {
            fm_lines.push(format!("  - {}", yaml_quote(t)));
        }
    } else {
        fm_lines.push("tags: []".to_string());
    }

    if cfg.frontmatter.add_slug_to_aliases && !slug.is_empty() {
        fm_lines.push("aliases:".to_string());
        fm_lines.push(format!("  - {}", yaml_quote(slug)));
    }

    fm_lines.push("---".to_string());
    format!("{}\n{}", fm_lines.join("\n"), final_body)
}

/// 根据模板生成不含扩展名的文件主名（若无摘要则自动退化为纯时间戳 ID）
pub fn format_filename_stem(template: &str, id: &str, slug: &str) -> String {
    if slug.is_empty() {
        let raw = template.replace("{id}", id).replace("{slug}", "");
        let trimmed = raw
            .trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.'))
            .to_string();
        if trimmed.is_empty() {
            id.to_string()
        } else {
            trimmed
        }
    } else {
        let rendered = template.replace("{id}", id).replace("{slug}", slug);
        let trimmed = rendered
            .trim_matches(|c: char| c.is_whitespace() || c == '.')
            .to_string();
        if trimmed.is_empty() {
            id.to_string()
        } else {
            trimmed
        }
    }
}

/// 在目标目录下解析不冲突的 .md 完整路径（ignore_existing 用于原地修订重命名时排除自身）
fn resolve_unique_md_path(dir: &Path, stem: &str, ignore_existing: Option<&Path>) -> PathBuf {
    let primary = dir.join(format!("{stem}.md"));
    if !primary.exists() || ignore_existing.is_some_and(|p| p == primary.as_path()) {
        return primary;
    }
    for idx in 1..10_000 {
        let candidate = dir.join(format!("{stem}_{idx}.md"));
        if !candidate.exists() || ignore_existing.is_some_and(|p| p == candidate.as_path()) {
            return candidate;
        }
    }
    dir.join(format!("{stem}_{}.md", Local::now().timestamp_millis()))
}

/// 执行归档落盘（兼容新建卡片与「唤回上一张」原地更新/清空撤回）
pub fn archive_session(
    cfg: &AppConfig,
    session: &mut DraftSession,
    content: String,
    target_index: usize,
) -> Result<ArchiveOutcome, String> {
    let start_ts = Instant::now();
    let safe_target_idx = if target_index < cfg.targets.len() {
        target_index
    } else {
        0
    };

    // 情况 A：处于「修订上一张卡片」模式
    if let Some(recalled) = session.active_recall.clone() {
        if content.trim().is_empty() {
            // 用户将刚保存的卡片内容清空后保存 -> 删除原误存卡片
            let deleted_str = recalled.file_path.to_string_lossy().into_owned();
            if recalled.file_path.exists() {
                if let Err(e) = fs::remove_file(&recalled.file_path) {
                    crate::log_error!(
                        "WRITER",
                        "Failed deleting recalled card '{}': {e}",
                        recalled.file_path.display()
                    );
                    return Err(format!("删除撤回卡片失败: {e}"));
                }
            }
            session.discard_current_draft();
            session.last_archived = None;
            crate::log_info!(
                "WRITER",
                "Deleted recalled card via empty-save: id='{}', path='{}' ({:.2}ms)",
                recalled.id,
                deleted_str,
                start_ts.elapsed().as_secs_f64() * 1000.0
            );
            return Ok(ArchiveOutcome::DeletedRecalled {
                deleted_path: deleted_str,
            });
        }

        let (target_meta, target_dir) = cfg.resolve_target_dir(safe_target_idx);
        if let Err(e) = fs::create_dir_all(&target_dir) {
            crate::log_error!(
                "WRITER",
                "Failed creating target dir '{}': {e}",
                target_dir.display()
            );
            return Err(format!("创建目标目录失败 ({}): {e}", target_dir.display()));
        }

        let slug = extract_slug(&content, cfg.naming.max_slug_chars);
        let stem = format_filename_stem(&cfg.naming.filename_template, &recalled.id, &slug);
        let dest_path = resolve_unique_md_path(&target_dir, &stem, Some(&recalled.file_path));
        let full_doc = build_markdown_document(cfg, &recalled.id, &recalled.created, &slug, &content);

        if let Err(e) = fs::write(&dest_path, full_doc.as_bytes()) {
            crate::log_error!(
                "WRITER",
                "Failed updating recalled card '{}': {e}",
                dest_path.display()
            );
            return Err(format!("更新卡片写入失败: {e}"));
        }

        let renamed = dest_path != recalled.file_path;
        if renamed && recalled.file_path.exists() {
            if let Err(e) = fs::remove_file(&recalled.file_path) {
                crate::log_warn!(
                    "WRITER",
                    "Updated card written to '{}', but failed removing old file '{}': {e}",
                    dest_path.display(),
                    recalled.file_path.display()
                );
            }
        }

        let filename = dest_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_path_str = dest_path.to_string_lossy().into_owned();

        crate::log_info!(
            "WRITER",
            "Updated recalled card: id='{}', target='{}', renamed={renamed}, chars={}, lines={}, file='{}' ({:.2}ms)",
            recalled.id,
            target_meta.name,
            content.chars().count(),
            content.lines().count(),
            file_path_str,
            start_ts.elapsed().as_secs_f64() * 1000.0
        );

        session.complete_archive(Some(LastArchivedCard {
            file_path: dest_path,
            id: recalled.id,
            created: recalled.created,
            raw_body: content,
            target_index: safe_target_idx,
        }));

        return Ok(ArchiveOutcome::Saved {
            file_path: file_path_str,
            filename,
            updated_existing: true,
        });
    }

    // 情况 B：普通新建模式，若全空白则跳过落盘并清理孤儿附件
    if content.trim().is_empty() {
        session.discard_current_draft();
        crate::log_debug!("WRITER", "Archive skipped because editor content is empty");
        return Ok(ArchiveOutcome::EmptySkipped);
    }

    let (target_meta, target_dir) = cfg.resolve_target_dir(safe_target_idx);
    if let Err(e) = fs::create_dir_all(&target_dir) {
        crate::log_error!(
            "WRITER",
            "Failed creating target dir '{}': {e}",
            target_dir.display()
        );
        return Err(format!("创建目标目录失败 ({}): {e}", target_dir.display()));
    }

    let now = Local::now();
    let id = now.format(&cfg.naming.id_format).to_string();
    let created = now.to_rfc3339_opts(SecondsFormat::Secs, false);
    let slug = extract_slug(&content, cfg.naming.max_slug_chars);
    let inline_tag_count = extract_inline_tags(&content).len();
    let stem = format_filename_stem(&cfg.naming.filename_template, &id, &slug);
    let dest_path = resolve_unique_md_path(&target_dir, &stem, None);
    let full_doc = build_markdown_document(cfg, &id, &created, &slug, &content);

    if let Err(e) = fs::write(&dest_path, full_doc.as_bytes()) {
        crate::log_error!(
            "WRITER",
            "Failed writing new card '{}': {e}",
            dest_path.display()
        );
        return Err(format!("写入卡片文件失败 ({}): {e}", dest_path.display()));
    }

    let filename = dest_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file_path_str = dest_path.to_string_lossy().into_owned();

    crate::log_info!(
        "WRITER",
        "Archived new card: id='{id}', target='{}', chars={}, lines={}, inline_tags={inline_tag_count}, file='{file_path_str}' ({:.2}ms)",
        target_meta.name,
        content.chars().count(),
        content.lines().count(),
        start_ts.elapsed().as_secs_f64() * 1000.0
    );

    session.complete_archive(Some(LastArchivedCard {
        file_path: dest_path,
        id,
        created,
        raw_body: content,
        target_index: safe_target_idx,
    }));

    Ok(ArchiveOutcome::Saved {
        file_path: file_path_str,
        filename,
        updated_existing: false,
    })
}

/// 检查系统剪贴板是否包含位图图像；若有则保存为 PNG 附件并返回 Obsidian 引用语法 `![[...png]]`
pub fn paste_clipboard_image_to_vault(
    cfg: &AppConfig,
    session: &mut DraftSession,
) -> Result<Option<String>, String> {
    let start_ts = Instant::now();
    let mut clipboard = arboard::Clipboard::new().map_err(|e| {
        crate::log_error!("CLIPBOARD", "Failed opening OS clipboard: {e}");
        format!("访问剪贴板失败: {e}")
    })?;
    let Ok(img_data) = clipboard.get_image() else {
        crate::log_debug!("CLIPBOARD", "Paste image requested, but clipboard contains no bitmap");
        return Ok(None);
    };

    if img_data.width == 0 || img_data.height == 0 {
        crate::log_warn!(
            "CLIPBOARD",
            "Clipboard returned empty image dimensions: {}x{}",
            img_data.width,
            img_data.height
        );
        return Ok(None);
    }

    let width = img_data.width as u32;
    let height = img_data.height as u32;
    let rgba = image::RgbaImage::from_raw(width, height, img_data.bytes.into_owned())
        .ok_or_else(|| {
            crate::log_error!(
                "CLIPBOARD",
                "Failed constructing RgbaImage from raw clipboard buffer ({width}x{height})"
            );
            "解析剪贴板位图缓冲区失败".to_string()
        })?;

    let attach_dir = cfg.resolve_attachments_dir();
    if let Err(e) = fs::create_dir_all(&attach_dir) {
        crate::log_error!(
            "CLIPBOARD",
            "Failed creating attachments dir '{}': {e}",
            attach_dir.display()
        );
        return Err(format!("创建附件目录失败 ({}): {e}", attach_dir.display()));
    }

    let stamp = Local::now().format("%Y%m%d%H%M%S").to_string();
    let base_stem = format!("Pasted image {stamp}");
    let mut final_name = format!("{base_stem}.png");
    let mut dest_path = attach_dir.join(&final_name);
    let mut suffix = 1usize;
    while dest_path.exists() {
        final_name = format!("{base_stem}_{suffix}.png");
        dest_path = attach_dir.join(&final_name);
        suffix += 1;
    }

    if let Err(e) = rgba.save_with_format(&dest_path, image::ImageFormat::Png) {
        crate::log_error!(
            "CLIPBOARD",
            "Failed encoding/saving PNG '{}': {e}",
            dest_path.display()
        );
        return Err(format!("保存截图 PNG 失败: {e}"));
    }

    let kb = fs::metadata(&dest_path)
        .map(|m| m.len() as f64 / 1024.0)
        .unwrap_or(0.0);
    crate::log_info!(
        "CLIPBOARD",
        "Saved pasted screenshot ({width}x{height}, {kb:.1} KB) -> '{}' ({:.2}ms)",
        dest_path.display(),
        start_ts.elapsed().as_secs_f64() * 1000.0
    );

    session.record_pasted_image(dest_path);
    Ok(Some(format!("![[{final_name}]]")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_cleans_markdown_links_tags_and_illegal_chars() {
        let input = "# 康威定律：组织/架构?的映射 [[20260101-微服务|微服务边界]] #系统架构 #读书\n正文第二行";
        let slug = extract_slug(input, 40);
        assert_eq!(slug, "康威定律：组织 架构 的映射 微服务边界");
    }

    #[test]
    fn slug_falls_back_to_empty_for_image_only_note() {
        let input = "![[Pasted image 20261125112245.png]]\n#截图";
        let slug = extract_slug(input, 40);
        assert_eq!(slug, "");
        assert_eq!(
            format_filename_stem("{id}-{slug}", "20261125112245", &slug),
            "20261125112245"
        );
    }

    #[test]
    fn inline_tags_ignore_code_headings_numbers_and_wikilinks() {
        let text = "# 一级标题\n思考 #架构/微服务 与 #2026复盘 但忽略 #12345 和 `code #fake` 以及 [[Card#Section]]\n```\n#in_code_block\n```";
        let tags = extract_inline_tags(text);
        assert_eq!(tags, vec!["架构/微服务".to_string(), "2026复盘".to_string()]);
    }

    #[test]
    fn archive_and_recall_update_and_rename_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("zettel_test_{}", Local::now().timestamp_nanos_opt().unwrap_or(1)));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let mut cfg = AppConfig::default();
        cfg.vault_dir = tmp.to_string_lossy().into_owned();
        let mut session = DraftSession::new(tmp.join("draft.json"));

        // 1. 新建卡片
        let res = archive_session(&cfg, &mut session, "第一版标题 #灵感\n内容行".to_string(), 0).unwrap();
        let first_path = match res {
            ArchiveOutcome::Saved { file_path, updated_existing, .. } => {
                assert!(!updated_existing);
                PathBuf::from(file_path)
            }
            _ => panic!("expected Saved"),
        };
        assert!(first_path.exists());
        let saved_text = fs::read_to_string(&first_path).unwrap();
        assert!(saved_text.contains("aliases:\n  - \"第一版标题\""));
        assert!(saved_text.contains("  - \"灵感\""));

        // 2. 唤回上一张并修改首行标题，验证保留原 ID 且自动重命名更新文件
        let recalled = session.begin_recall_last().unwrap();
        let orig_id = recalled.id.clone();
        let res2 = archive_session(&cfg, &mut session, "修订后的新标题 #灵感 #补充\n补充了一句话".to_string(), 0).unwrap();
        let second_path = match res2 {
            ArchiveOutcome::Saved { file_path, updated_existing, .. } => {
                assert!(updated_existing);
                PathBuf::from(file_path)
            }
            _ => panic!("expected updated Saved"),
        };
        assert!(!first_path.exists(), "old file should be cleanly renamed/removed");
        assert!(second_path.exists());
        let updated_text = fs::read_to_string(&second_path).unwrap();
        assert!(updated_text.contains(&format!("id: \"{orig_id}\"")));
        assert!(updated_text.contains("aliases:\n  - \"修订后的新标题\""));
        assert!(updated_text.contains("  - \"补充\""));

        let _ = fs::remove_dir_all(&tmp);
    }
}
