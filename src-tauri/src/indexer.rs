use crate::config::AppConfig;
use crate::writer::{extract_inline_tags, split_leading_frontmatter};
use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as NucleoConfig, Matcher, Utf32Str};
use parking_lot::RwLock;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

const MAX_HEADER_SCAN_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
struct IndexedCard {
    stem: String,
    rel_path: String,
    display_title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    mtime_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CardSuggestion {
    pub stem: String,
    pub display_title: String,
    pub rel_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagSuggestion {
    pub tag: String,
    pub count: usize,
}

#[derive(Default)]
struct IndexStore {
    vault_root: PathBuf,
    ignore_dirs: HashSet<String>,
    extra_tags: Vec<String>,
    cards: HashMap<PathBuf, IndexedCard>,
    tag_counts: HashMap<String, usize>,
}

pub struct VaultIndexer {
    store: Arc<RwLock<IndexStore>>,
    watcher: RwLock<Option<RecommendedWatcher>>,
}

impl VaultIndexer {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(IndexStore::default())),
            watcher: RwLock::new(None),
        }
    }

    /// 根据当前配置全量扫描知识库并启动实时文件增量监听
    pub fn rebuild_and_watch(&self, cfg: &AppConfig) {
        let start_ts = Instant::now();
        let vault_root = cfg.vault_path();
        let ignore_dirs: HashSet<String> = cfg
            .ignore_dirs
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let extra_tags: Vec<String> = cfg
            .frontmatter
            .extra_tags
            .iter()
            .map(|t| t.trim().trim_start_matches('#').to_string())
            .filter(|t| !t.is_empty())
            .collect();

        let mut cards = HashMap::new();
        if vault_root.exists() {
            scan_dir_recursive(&vault_root, &vault_root, &ignore_dirs, &mut cards);
        } else {
            crate::log_warn!(
                "INDEXER",
                "Vault directory '{}' does not exist during full scan",
                vault_root.display()
            );
        }

        let (card_count, tag_count) = {
            let mut guard = self.store.write();
            guard.vault_root = vault_root.clone();
            guard.ignore_dirs = ignore_dirs;
            guard.extra_tags = extra_tags;
            guard.cards = cards;
            recompute_tag_counts(&mut guard);
            (guard.cards.len(), guard.tag_counts.len())
        };

        crate::log_info!(
            "INDEXER",
            "Full index rebuild complete: vault='{}', cards={card_count}, tags={tag_count} ({:.2}ms)",
            vault_root.display(),
            start_ts.elapsed().as_secs_f64() * 1000.0
        );

        self.start_watcher(&vault_root);
    }

    fn start_watcher(&self, vault_root: &Path) {
        // 先释放旧的 watcher
        *self.watcher.write() = None;
        if !vault_root.exists() {
            return;
        }

        let store_ref = Arc::clone(&self.store);
        let watcher_res = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| match res {
                Ok(event) => match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                        let mut guard = store_ref.write();
                        let mut changed = false;
                        for path in &event.paths {
                            if should_ignore_path(&guard.vault_root, path, &guard.ignore_dirs) {
                                continue;
                            }
                            if is_markdown_file(path) {
                                if path.exists() {
                                    if let Some(card) = parse_card_file(&guard.vault_root, path) {
                                        crate::log_debug!(
                                            "INDEXER",
                                            "Watcher upserted card '{}'",
                                            card.rel_path
                                        );
                                        guard.cards.insert(path.clone(), card);
                                        changed = true;
                                    }
                                } else if guard.cards.remove(path).is_some() {
                                    crate::log_debug!(
                                        "INDEXER",
                                        "Watcher removed deleted card '{}'",
                                        path.display()
                                    );
                                    changed = true;
                                }
                            } else if !path.exists() {
                                // 目录被删除或改名时清理该前缀下的所有已索引卡片
                                let before = guard.cards.len();
                                guard.cards.retain(|k, _| !k.starts_with(path));
                                let removed = before.saturating_sub(guard.cards.len());
                                if removed > 0 {
                                    crate::log_debug!(
                                        "INDEXER",
                                        "Watcher pruned {removed} cards under removed dir '{}'",
                                        path.display()
                                    );
                                    changed = true;
                                }
                            }
                        }
                        if changed {
                            recompute_tag_counts(&mut guard);
                        }
                    }
                    _ => {}
                },
                Err(err) => {
                    crate::log_warn!("INDEXER", "Filesystem watcher event error: {err}");
                }
            },
            NotifyConfig::default(),
        );

        match watcher_res {
            Ok(mut w) => match w.watch(vault_root, RecursiveMode::Recursive) {
                Ok(_) => {
                    crate::log_info!(
                        "INDEXER",
                        "Started recursive filesystem watcher on '{}'",
                        vault_root.display()
                    );
                    *self.watcher.write() = Some(w);
                }
                Err(e) => {
                    crate::log_error!(
                        "INDEXER",
                        "Failed watching vault directory '{}': {e}",
                        vault_root.display()
                    );
                }
            },
            Err(e) => {
                crate::log_error!("INDEXER", "Failed creating RecommendedWatcher: {e}");
            }
        }
    }

    /// 供归档写入后立即同步单文件索引，做到 0ms 立即可搜
    pub fn upsert_single_file(&self, path: &Path) {
        let mut guard = self.store.write();
        if should_ignore_path(&guard.vault_root, path, &guard.ignore_dirs) {
            return;
        }
        if path.exists() && is_markdown_file(path) {
            if let Some(card) = parse_card_file(&guard.vault_root, path) {
                guard.cards.insert(path.to_path_buf(), card);
                recompute_tag_counts(&mut guard);
            }
        }
    }

    pub fn remove_single_file(&self, path: &Path) {
        let mut guard = self.store.write();
        if guard.cards.remove(path).is_some() {
            recompute_tag_counts(&mut guard);
        }
    }

    /// 模糊搜索全库卡片（输入为空时按最近修改时间倒序返回最近卡片）
    pub fn search_cards(&self, query: &str, limit: usize) -> Vec<CardSuggestion> {
        let max_items = limit.clamp(1, 30);
        let guard = self.store.read();
        let q = query.trim();

        if q.is_empty() {
            let mut recent: Vec<&IndexedCard> = guard.cards.values().collect();
            recent.sort_by(|a, b| b.mtime_secs.cmp(&a.mtime_secs).then_with(|| a.stem.cmp(&b.stem)));
            return recent
                .into_iter()
                .take(max_items)
                .map(|c| CardSuggestion {
                    stem: c.stem.clone(),
                    display_title: c.display_title.clone(),
                    rel_path: c.rel_path.clone(),
                })
                .collect();
        }

        let mut matcher = Matcher::new(NucleoConfig::DEFAULT);
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, u64, &IndexedCard)> = Vec::new();

        for card in guard.cards.values() {
            buf.clear();
            let title_score = pattern
                .score(Utf32Str::new(&card.display_title, &mut buf), &mut matcher)
                .map(|s| s.saturating_add(15));

            buf.clear();
            let stem_score = pattern.score(Utf32Str::new(&card.stem, &mut buf), &mut matcher);

            let alias_score = card
                .aliases
                .iter()
                .filter_map(|alias| {
                    buf.clear();
                    pattern
                        .score(Utf32Str::new(alias, &mut buf), &mut matcher)
                        .map(|s| s.saturating_add(10))
                })
                .max();

            if let Some(best) = [title_score, stem_score, alias_score]
                .into_iter()
                .flatten()
                .max()
            {
                scored.push((best, card.mtime_secs, card));
            }
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.stem.cmp(&b.2.stem))
        });

        scored
            .into_iter()
            .take(max_items)
            .map(|(_, _, c)| CardSuggestion {
                stem: c.stem.clone(),
                display_title: c.display_title.clone(),
                rel_path: c.rel_path.clone(),
            })
            .collect()
    }

    /// 模糊搜索全库 #标签（输入为空时按使用频次倒序返回高频标签）
    pub fn search_tags(&self, query: &str, limit: usize) -> Vec<TagSuggestion> {
        let max_items = limit.clamp(1, 30);
        let guard = self.store.read();
        let q = query.trim().trim_start_matches('#');

        if q.is_empty() {
            let mut list: Vec<(&String, &usize)> = guard.tag_counts.iter().collect();
            list.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            return list
                .into_iter()
                .take(max_items)
                .map(|(tag, &count)| TagSuggestion {
                    tag: tag.clone(),
                    count,
                })
                .collect();
        }

        let mut matcher = Matcher::new(NucleoConfig::DEFAULT);
        let pattern = Pattern::parse(q, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize, &String)> = Vec::new();

        for (tag, &count) in &guard.tag_counts {
            buf.clear();
            if let Some(score) = pattern.score(Utf32Str::new(tag, &mut buf), &mut matcher) {
                scored.push((score, count, tag));
            }
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(b.2))
        });

        scored
            .into_iter()
            .take(max_items)
            .map(|(_, count, tag)| TagSuggestion {
                tag: tag.clone(),
                count,
            })
            .collect()
    }
}

fn recompute_tag_counts(store: &mut IndexStore) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for extra in &store.extra_tags {
        counts.entry(extra.clone()).or_insert(0);
    }
    for card in store.cards.values() {
        for tag in &card.tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    store.tag_counts = counts;
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

fn should_ignore_path(vault_root: &Path, path: &Path, ignore_dirs: &HashSet<String>) -> bool {
    let rel = path.strip_prefix(vault_root).unwrap_or(path);
    for comp in rel.components() {
        let name = comp.as_os_str().to_string_lossy();
        if name.starts_with('.') && name.len() > 1 {
            return true;
        }
        if ignore_dirs.contains(&name.to_ascii_lowercase()) {
            return true;
        }
    }
    false
}

fn scan_dir_recursive(
    vault_root: &Path,
    dir: &Path,
    ignore_dirs: &HashSet<String>,
    out: &mut HashMap<PathBuf, IndexedCard>,
) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if should_ignore_path(vault_root, &path, ignore_dirs) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            scan_dir_recursive(vault_root, &path, ignore_dirs, out);
        } else if ft.is_file() && is_markdown_file(&path) {
            if let Some(card) = parse_card_file(vault_root, &path) {
                out.insert(path, card);
            }
        }
    }
}

/// 剥离文件名前导纯数字时间戳 ID（如 `20261125112230-康威定律` -> `康威定律`）
fn strip_timestamp_prefix(stem: &str) -> &str {
    let digits = stem.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits >= 8 {
        let rest = stem[digits..].trim_start_matches(['-', '_', ' ']);
        if !rest.is_empty() {
            return rest;
        }
    }
    stem
}

#[derive(Default)]
struct ParsedFrontmatterMeta {
    title: Option<String>,
    aliases: Vec<String>,
    tags: Vec<String>,
}

/// 轻量高速解析 YAML Frontmatter 中的 title / aliases / tags（兼容行内数组 `[a, b]` 与多行 `- item`）
fn parse_frontmatter_meta(fm: &str) -> ParsedFrontmatterMeta {
    let mut meta = ParsedFrontmatterMeta::default();
    let mut active_list_key: Option<&str> = None;

    for raw_line in fm.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(list_key) = active_list_key {
            if let Some(item) = trimmed.strip_prefix('-') {
                let val = unquote_yaml_scalar(item.trim());
                if !val.is_empty() {
                    match list_key {
                        "aliases" => meta.aliases.push(val),
                        "tags" => {
                            let clean = val.trim_start_matches('#').to_string();
                            if !clean.is_empty() {
                                meta.tags.push(clean);
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            } else if !raw_line.starts_with([' ', '\t']) {
                active_list_key = None;
            }
        }

        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();

        match key.as_str() {
            "title" => {
                let s = unquote_yaml_scalar(val);
                if !s.is_empty() {
                    meta.title = Some(s);
                }
            }
            "alias" | "aliases" => {
                if val.is_empty() {
                    active_list_key = Some("aliases");
                } else {
                    meta.aliases.extend(parse_inline_yaml_list_or_scalar(val));
                }
            }
            "tag" | "tags" => {
                if val.is_empty() {
                    active_list_key = Some("tags");
                } else {
                    for item in parse_inline_yaml_list_or_scalar(val) {
                        for sub in item.split([',', ' ']) {
                            let clean = sub.trim().trim_start_matches('#').to_string();
                            if !clean.is_empty() {
                                meta.tags.push(clean);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    meta
}

fn unquote_yaml_scalar(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        return t[1..t.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    t.to_string()
}

fn parse_inline_yaml_list_or_scalar(val: &str) -> Vec<String> {
    let t = val.trim();
    if t.starts_with('[') && t.ends_with(']') {
        let inner = &t[1..t.len() - 1];
        inner
            .split(',')
            .map(unquote_yaml_scalar)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        let s = unquote_yaml_scalar(t);
        if s.is_empty() {
            Vec::new()
        } else {
            vec![s]
        }
    }
}

fn read_utf8_prefix(path: &Path, max_bytes: usize) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(4096);
    file.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            String::from_utf8(bytes[..valid].to_vec()).ok()
        }
    }
}

fn parse_card_file(vault_root: &Path, path: &Path) -> Option<IndexedCard> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let rel_path = path
        .strip_prefix(vault_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    let mtime_secs = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let content = read_utf8_prefix(path, MAX_HEADER_SCAN_BYTES).unwrap_or_default();
    let (fm_meta, body) = if let Some((fm, rest)) = split_leading_frontmatter(&content) {
        (parse_frontmatter_meta(fm), rest)
    } else {
        (ParsedFrontmatterMeta::default(), content.as_str())
    };

    let first_heading = body.lines().find_map(|line| {
        let t = line.trim_start();
        t.strip_prefix("# ").map(|h| h.trim().to_string())
    });

    let display_title = fm_meta
        .aliases
        .first()
        .cloned()
        .or_else(|| fm_meta.title.clone())
        .or(first_heading)
        .unwrap_or_else(|| strip_timestamp_prefix(&stem).to_string());

    let mut tag_set = HashSet::new();
    let mut tags = Vec::new();
    for t in fm_meta.tags.into_iter().chain(extract_inline_tags(body)) {
        if tag_set.insert(t.clone()) {
            tags.push(t);
        }
    }

    Some(IndexedCard {
        stem,
        rel_path,
        display_title,
        aliases: fm_meta.aliases,
        tags,
        mtime_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexer_searches_chinese_aliases_and_tags() {
        let tmp = std::env::temp_dir().join("zettel_indexer_unit_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("00-Inbox")).unwrap();
        fs::create_dir_all(tmp.join(".obsidian")).unwrap();

        fs::write(
            tmp.join("00-Inbox/20261125100000-卢曼卡片盒原则.md"),
            "---\nid: \"20261125100000\"\ntags:\n  - \"知识管理\"\naliases:\n  - \"卢曼卡片盒原子化原则\"\n---\n正文内容 #双链笔记\n",
        )
        .unwrap();

        // .obsidian 下的文件应当被忽略
        fs::write(tmp.join(".obsidian/hidden.md"), "# 不应被索引").unwrap();

        let mut cfg = AppConfig::default();
        cfg.vault_dir = tmp.to_string_lossy().into_owned();

        let indexer = VaultIndexer::new();
        indexer.rebuild_and_watch(&cfg);

        let cards = indexer.search_cards("原子化", 8);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].stem, "20261125100000-卢曼卡片盒原则");
        assert_eq!(cards[0].display_title, "卢曼卡片盒原子化原则");

        let tags = indexer.search_tags("", 10);
        let tag_names: Vec<String> = tags.into_iter().map(|t| t.tag).collect();
        assert!(tag_names.contains(&"知识管理".to_string()));
        assert!(tag_names.contains(&"双链笔记".to_string()));

        let _ = fs::remove_dir_all(&tmp);
    }
}
