use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    pub vault_dir: String,
    pub targets: Vec<TargetDir>,
    pub attachments_dir: String,
    pub ignore_dirs: Vec<String>,
    pub hotkey: HotkeyConfig,
    pub naming: NamingConfig,
    pub frontmatter: FrontmatterConfig,
    pub ui: UiConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub level: String,
    pub max_file_size_mb: u64,
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: "info".to_string(),
            max_file_size_mb: 5,
            max_files: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetDir {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub toggle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NamingConfig {
    pub id_format: String,
    pub filename_template: String,
    pub max_slug_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FrontmatterConfig {
    pub enabled: bool,
    pub include_id: bool,
    pub include_created: bool,
    pub sync_inline_tags: bool,
    pub add_slug_to_aliases: bool,
    pub extra_tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiConfig {
    pub theme: String,
    pub width: u32,
    pub height: u32,
    pub font_family: String,
    pub font_size: u32,
    pub always_on_top_default: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            vault_dir: String::new(),
            targets: vec![
                TargetDir {
                    name: "闪念盒".to_string(),
                    path: "00-Inbox".to_string(),
                },
                TargetDir {
                    name: "永久卡".to_string(),
                    path: "10-Permanent".to_string(),
                },
            ],
            attachments_dir: "attachments".to_string(),
            ignore_dirs: vec![
                ".obsidian".to_string(),
                ".git".to_string(),
                ".trash".to_string(),
                ".vscode".to_string(),
                "node_modules".to_string(),
                "attachments".to_string(),
            ],
            hotkey: HotkeyConfig::default(),
            naming: NamingConfig::default(),
            frontmatter: FrontmatterConfig::default(),
            ui: UiConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle: "Ctrl+Alt+N".to_string(),
        }
    }
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            id_format: "%Y%m%d%H%M%S".to_string(),
            filename_template: "{id}-{slug}".to_string(),
            max_slug_chars: 40,
        }
    }
}

impl Default for FrontmatterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_id: true,
            include_created: true,
            sync_inline_tags: true,
            add_slug_to_aliases: true,
            extra_tags: Vec::new(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            width: 640,
            height: 360,
            font_family: "Microsoft YaHei UI, Cascadia Mono, Segoe UI, sans-serif".to_string(),
            font_size: 16,
            always_on_top_default: true,
        }
    }
}

fn relative_subpath_segments(raw: &str) -> Vec<&str> {
    let mut segments: Vec<&str> = Vec::new();
    for part in raw.trim().split(['\\', '/']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            segments.pop();
            continue;
        }
        segments.push(part);
    }
    segments
}

pub fn normalize_relative_subpath(raw: &str) -> String {
    relative_subpath_segments(raw).join(std::path::MAIN_SEPARATOR_STR)
}

fn is_toml_bare_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_windows_path_key(key: &str) -> bool {
    matches!(key, "vault_dir" | "path" | "attachments_dir")
}

/// 预处理 TOML 文本中路径字段（`vault_dir` / `path` / `attachments_dir`）的双引号字符串：
/// 允许用户在双引号内直接书写单反斜杠 Windows 路径（如 `"00. 笔记盒\00-Inbox"`、`"aaa\bbb"`），
/// 避免 TOML 将 `\0` 报非法转义或将 `\b`/`\n`/`\t` 静默解析为控制字符；
/// 同时完全兼容已双写转义的 `"a\\b"`、单引号字面量 `'a\b'` 与正斜杠 `"a/b"`。
fn preprocess_toml_windows_paths(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + 16);
    let mut i = 0;
    let mut expect_key = true;

    while i < len {
        let b = bytes[i];

        // 1. 注释：原样保留至行尾
        if b == b'#' {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            out.push_str(&raw[start..i]);
            continue;
        }

        // 2. 换行：重置为可出现键名的位置
        if b == b'\n' || b == b'\r' {
            out.push(b as char);
            i += 1;
            expect_key = true;
            continue;
        }

        // 3. 行内空白：保持当前位置语义
        if b == b' ' || b == b'\t' {
            out.push(b as char);
            i += 1;
            continue;
        }

        // 4. 内联表起始或成员分隔符：后续可能出现键名（如 `{ name = "...", path = "..." }`）
        if b == b'{' || b == b',' {
            out.push(b as char);
            i += 1;
            expect_key = true;
            continue;
        }

        // 5. 多行基本字符串 """..."""：原样跳过
        if i + 2 < len && &bytes[i..i + 3] == b"\"\"\"" {
            let start = i;
            i += 3;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if i + 2 < len && &bytes[i..i + 3] == b"\"\"\"" {
                    i += 3;
                    break;
                }
                i += 1;
            }
            out.push_str(&raw[start..i]);
            expect_key = false;
            continue;
        }

        // 6. 多行字面字符串 '''...'''：原样跳过
        if i + 2 < len && &bytes[i..i + 3] == b"'''" {
            let start = i;
            i += 3;
            while i < len {
                if i + 2 < len && &bytes[i..i + 3] == b"'''" {
                    i += 3;
                    break;
                }
                i += 1;
            }
            out.push_str(&raw[start..i]);
            expect_key = false;
            continue;
        }

        // 7. 单行基本字符串（非路径键）：原样跳过
        if b == b'"' {
            let start = i;
            i += 1;
            while i < len && bytes[i] != b'\n' && bytes[i] != b'\r' {
                if bytes[i] == b'\\' && i + 1 < len && bytes[i + 1] != b'\n' && bytes[i + 1] != b'\r' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&raw[start..i]);
            expect_key = false;
            continue;
        }

        // 8. 单行字面字符串 '...'：原样跳过
        if b == b'\'' {
            let start = i;
            i += 1;
            while i < len && bytes[i] != b'\n' && bytes[i] != b'\r' {
                if bytes[i] == b'\'' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&raw[start..i]);
            expect_key = false;
            continue;
        }

        // 9. 键名位置：识别 `vault_dir` / `path` / `attachments_dir = "..."`
        if expect_key && is_toml_bare_key_byte(b) {
            let key_start = i;
            while i < len && is_toml_bare_key_byte(bytes[i]) {
                i += 1;
            }
            let key = &raw[key_start..i];

            if is_windows_path_key(key) {
                let mut look = i;
                while look < len && (bytes[look] == b' ' || bytes[look] == b'\t') {
                    look += 1;
                }
                if look < len && bytes[look] == b'=' {
                    look += 1;
                    while look < len && (bytes[look] == b' ' || bytes[look] == b'\t') {
                        look += 1;
                    }
                    let is_single_line_double_quote = look < len
                        && bytes[look] == b'"'
                        && !(look + 2 < len && &bytes[look..look + 3] == b"\"\"\"");

                    if is_single_line_double_quote {
                        // 原样输出 `key = "`
                        out.push_str(&raw[key_start..=look]);
                        let mut pos = look + 1;
                        let mut chunk_start = pos;

                        while pos < len && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
                            if bytes[pos] == b'\\' {
                                out.push_str(&raw[chunk_start..pos]);
                                if pos + 1 < len && bytes[pos + 1] == b'\\' {
                                    // 保留用户已双写的 `\\`
                                    out.push_str("\\\\");
                                    pos += 2;
                                } else {
                                    // 单反斜杠（含 `\0`、`\b`、`\n`、`\t` 或尾部 `\"`）统一转义为 `\\`
                                    out.push_str("\\\\");
                                    pos += 1;
                                }
                                chunk_start = pos;
                                continue;
                            }
                            if bytes[pos] == b'"' {
                                out.push_str(&raw[chunk_start..=pos]);
                                pos += 1;
                                chunk_start = pos;
                                break;
                            }
                            pos += 1;
                        }
                        if chunk_start < pos {
                            out.push_str(&raw[chunk_start..pos]);
                        }
                        i = pos;
                        expect_key = false;
                        continue;
                    }
                }
            }

            out.push_str(key);
            expect_key = false;
            continue;
        }

        // 10. 其他字符（含完整 UTF-8 多字节字符）
        if let Some(ch) = raw[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
        expect_key = false;
    }

    out
}

impl AppConfig {
    pub fn normalized(mut self) -> Self {
        self.vault_dir = self.vault_dir.trim().to_string();
        for target in &mut self.targets {
            target.path = normalize_relative_subpath(&target.path);
        }
        self.attachments_dir = normalize_relative_subpath(&self.attachments_dir);
        if self.targets.is_empty() {
            self.targets.push(TargetDir {
                name: "根目录".to_string(),
                path: String::new(),
            });
        }
        if self.naming.max_slug_chars == 0 {
            self.naming.max_slug_chars = 40;
        }
        if self.naming.id_format.trim().is_empty() {
            self.naming.id_format = "%Y%m%d%H%M%S".to_string();
        }
        if self.naming.filename_template.trim().is_empty() {
            self.naming.filename_template = "{id}-{slug}".to_string();
        }
        self.ui.width = self.ui.width.clamp(380, 1920);
        self.ui.height = self.ui.height.clamp(200, 1400);
        self.ui.font_size = self.ui.font_size.clamp(12, 32);
        if self.logging.level.trim().is_empty() {
            self.logging.level = "info".to_string();
        }
        self.logging.max_file_size_mb = self.logging.max_file_size_mb.clamp(1, 100);
        self.logging.max_files = self.logging.max_files.clamp(1, 10);
        self
    }

    pub fn vault_path(&self) -> PathBuf {
        PathBuf::from(&self.vault_dir)
    }

    pub fn resolve_target_dir(&self, index: usize) -> (&TargetDir, PathBuf) {
        let safe_idx = if index < self.targets.len() { index } else { 0 };
        let target = &self.targets[safe_idx];
        let mut full = self.vault_path();
        for seg in relative_subpath_segments(&target.path) {
            full.push(seg);
        }
        (target, full)
    }

    pub fn resolve_attachments_dir(&self) -> PathBuf {
        let mut full = self.vault_path();
        for seg in relative_subpath_segments(&self.attachments_dir) {
            full.push(seg);
        }
        full
    }
}

pub fn portable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn config_file_path() -> PathBuf {
    if let Ok(override_path) = std::env::var("ZETTEL_CONFIG_PATH") {
        if !override_path.trim().is_empty() {
            return PathBuf::from(override_path);
        }
    }
    portable_dir().join("config.toml")
}

pub fn local_cache_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join("ZettelQuick");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn webview_data_dir() -> PathBuf {
    let dir = local_cache_dir().join("WebView2");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn draft_cache_path() -> PathBuf {
    local_cache_dir().join("draft.json")
}

pub fn render_commented_config_toml(vault_dir: &Path) -> String {
    let vault_str = vault_dir.to_string_lossy().replace('\'', "''");
    format!(
        r#"# ============================================================================
# ZettelQuick 配置文件（修改并保存本文件后会自动热重载生效）
# 提示：Windows 路径建议使用单引号 '...' 包裹，无需双写反斜杠
# ============================================================================

# 知识库根目录（全库 [[双链]] 与 #标签 索引范围）
vault_dir = '{vault_str}'

# 可切换的落盘目标子目录（相对 vault_dir，支持多级子目录如 "00. 笔记盒\00-Inbox" 或 "aaa/bbb"；设为空字符串 "" 表示直接存入根目录）
# 第一项为默认落盘目录，在编辑器内按 Ctrl+Tab 可循环切换
targets = [
  {{ name = "闪念盒", path = "00-Inbox" }},
  {{ name = "永久卡", path = "10-Permanent" }},
]

# 粘贴截图自动保存目录（相对 vault_dir，支持多级子目录）
attachments_dir = "attachments"

# 全库索引时自动忽略的目录名
ignore_dirs = [".obsidian", ".git", ".trash", ".vscode", "node_modules", "attachments"]

[hotkey]
# 全局呼出 / 隐藏切换快捷键
toggle = "Ctrl+Alt+N"

[naming]
# 时间戳 ID 格式（%Y%m%d%H%M%S = 14位精确到秒，%Y%m%d%H%M = 12位精确到分）
id_format = "%Y%m%d%H%M%S"
# 有标题摘要时的文件名模板（可用变量：{{id}}, {{slug}}）
filename_template = "{{id}}-{{slug}}"
# 首行智能摘要最大保留字符数
max_slug_chars = 40

[frontmatter]
enabled = true
include_id = true
include_created = true
sync_inline_tags = true
add_slug_to_aliases = true
# 每张卡片固定追加的标签（例如 ["fleeting"]）
extra_tags = []

[ui]
# 主题模式："system" | "dark" | "light"
theme = "system"
width = 640
height = 360
font_family = "Microsoft YaHei UI, Cascadia Mono, Segoe UI, sans-serif"
font_size = 16
# 呼出笔记窗口时默认固定在所有窗口最外层（可通过 Ctrl+Shift+P 取消置顶并退到其他窗口后面）
always_on_top_default = true

[logging]
# 行为与诊断日志（默认输出至 %LOCALAPPDATA%\ZettelQuick\logs\zettel-quick.log，可通过托盘菜单直接打开）
enabled = true
# 日志详细程度："debug" | "info" | "warn" | "error"（遇到疑难问题可临时改为 "debug" 保存即刻生效）
level = "info"
# 单个日志文件轮转上限（MB）与保留的历史归档份数
max_file_size_mb = 5
max_files = 3
"#
    )
}

pub fn parse_config_str(raw: &str) -> Result<AppConfig, String> {
    let preprocessed = preprocess_toml_windows_paths(raw);
    toml::from_str::<AppConfig>(&preprocessed)
        .map(AppConfig::normalized)
        .map_err(|e| format!("TOML 解析失败: {e}"))
}

/// 加载同级 config.toml；若首次运行不存在，则弹出系统原生文件夹选择框并生成默认配置。
/// 返回 (配置实例, 是否为首次初始化生成)。
pub fn load_or_init_config() -> (AppConfig, bool) {
    let cfg_path = config_file_path();
    crate::log_info!("CONFIG", "Loading configuration from '{}'", cfg_path.display());

    if cfg_path.exists() {
        match fs::read_to_string(&cfg_path) {
            Ok(raw) => match parse_config_str(&raw) {
                Ok(cfg) if !cfg.vault_dir.trim().is_empty() => {
                    if let Err(e) = fs::create_dir_all(cfg.vault_path()) {
                        crate::log_warn!(
                            "CONFIG",
                            "Failed to ensure vault_dir '{}': {e}",
                            cfg.vault_dir
                        );
                    }
                    crate::log_info!(
                        "CONFIG",
                        "Loaded config OK: vault_dir='{}', targets={}, hotkey='{}', log_level='{}'",
                        cfg.vault_dir,
                        cfg.targets.len(),
                        cfg.hotkey.toggle,
                        cfg.logging.level
                    );
                    return (cfg, false);
                }
                Ok(_) => {
                    crate::log_warn!(
                        "CONFIG",
                        "Existing config '{}' has empty vault_dir -> triggering folder selection",
                        cfg_path.display()
                    );
                }
                Err(err) => {
                    // 保留用户已存在的 config.toml 不做覆盖破坏，回退至临时安全配置并在日志记录精准行号错误
                    crate::log_error!(
                        "CONFIG",
                        "Syntax error in existing '{}': {err}. Preserving file on disk and running with fallback defaults.",
                        cfg_path.display()
                    );
                    let mut fallback = AppConfig::default();
                    let default_vault = portable_dir().join("cards");
                    let _ = fs::create_dir_all(&default_vault);
                    fallback.vault_dir = default_vault.to_string_lossy().into_owned();
                    return (fallback.normalized(), false);
                }
            },
            Err(err) => {
                crate::log_error!(
                    "CONFIG",
                    "Failed to read existing config '{}': {err}",
                    cfg_path.display()
                );
            }
        }
    }

    // 首次启动：优先读取环境变量（便于测试），否则调用原生选目录对话框
    crate::log_info!("CONFIG", "First-run onboarding: prompting user for vault directory");
    let chosen_vault = std::env::var("ZETTEL_VAULT_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| {
            crate::log_info!("CONFIG", "Using ZETTEL_VAULT_DIR override: '{v}'");
            PathBuf::from(v)
        })
        .or_else(|| {
            rfd::FileDialog::new()
                .set_title("请选择你的卡片盒（Obsidian）知识库根目录")
                .pick_folder()
        })
        .unwrap_or_else(|| {
            let fallback_dir = portable_dir().join("cards");
            crate::log_warn!(
                "CONFIG",
                "Folder picker cancelled -> falling back to '{}'",
                fallback_dir.display()
            );
            fallback_dir
        });

    if let Err(e) = fs::create_dir_all(&chosen_vault) {
        crate::log_error!(
            "CONFIG",
            "Failed to create vault directory '{}': {e}",
            chosen_vault.display()
        );
    }
    let toml_text = render_commented_config_toml(&chosen_vault);
    match fs::write(&cfg_path, &toml_text) {
        Ok(_) => crate::log_info!(
            "CONFIG",
            "Generated initial config.toml at '{}' (vault='{}')",
            cfg_path.display(),
            chosen_vault.display()
        ),
        Err(e) => crate::log_error!(
            "CONFIG",
            "Failed to write initial config.toml at '{}': {e}",
            cfg_path.display()
        ),
    }

    let cfg = parse_config_str(&toml_text).unwrap_or_else(|e| {
        crate::log_error!("CONFIG", "Unexpected error parsing generated config: {e}");
        let mut fallback = AppConfig::default();
        fallback.vault_dir = chosen_vault.to_string_lossy().into_owned();
        fallback.normalized()
    });
    (cfg, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_commented_toml_roundtrips_windows_paths() {
        let win_path = Path::new(r"D:\Notes\new_vault\MyZettelkasten");
        let rendered = render_commented_config_toml(win_path);
        let parsed = parse_config_str(&rendered).expect("rendered config must parse cleanly");
        assert_eq!(parsed.vault_dir, r"D:\Notes\new_vault\MyZettelkasten");
        assert_eq!(parsed.targets.len(), 2);
        assert_eq!(parsed.hotkey.toggle, "Ctrl+Alt+N");
    }

    #[test]
    fn nested_target_subdirectories_with_windows_backslashes_in_double_quotes() {
        let raw = r#"
# 知识库根目录（全库 [[双链]] 与 #标签 索引范围）
vault_dir = 'C:\Users\wangh\OneDrive\Notebook'

# 可切换的落盘目标子目录（相对 vault_dir；设为空字符串 "" 表示直接存入根目录）
# 第一项为默认落盘目录，在编辑器内按 Ctrl+Tab 可循环切换
targets = [
  { name = "闪念盒", path = "00. 笔记盒\00-Inbox" },
  { name = "文献卡", path = "00. 笔记盒\10-Literature" },
  { name = "永久卡", path = "00. 笔记盒\20-Permanent" },
  { name = "other", path = "aaa\bbb" },
]
"#;
        let cfg = parse_config_str(raw).expect("nested backslash target paths must parse cleanly");
        assert_eq!(cfg.vault_dir, r"C:\Users\wangh\OneDrive\Notebook");
        assert_eq!(cfg.targets.len(), 4);

        let expected_rel = [
            normalize_relative_subpath("00. 笔记盒/00-Inbox"),
            normalize_relative_subpath("00. 笔记盒/10-Literature"),
            normalize_relative_subpath("00. 笔记盒/20-Permanent"),
            normalize_relative_subpath("aaa/bbb"),
        ];
        for (idx, exp) in expected_rel.iter().enumerate() {
            assert_eq!(&cfg.targets[idx].path, exp);
        }

        let (_, dir0) = cfg.resolve_target_dir(0);
        assert_eq!(
            dir0,
            PathBuf::from(r"C:\Users\wangh\OneDrive\Notebook")
                .join("00. 笔记盒")
                .join("00-Inbox")
        );

        let (_, dir3) = cfg.resolve_target_dir(3);
        assert_eq!(
            dir3,
            PathBuf::from(r"C:\Users\wangh\OneDrive\Notebook")
                .join("aaa")
                .join("bbb")
        );
    }

    #[test]
    fn target_paths_handle_escapes_leading_slashes_trailing_slashes_and_variants() {
        let raw = r#"
vault_dir = "C:\Users\wangh\OneDrive\Notebook"
attachments_dir = "00. 笔记盒\attachments\images\"

targets = [
  { name = "前导反斜杠", path = "\00. 笔记盒\00-Inbox" },
  { name = "正斜杠多级", path = "/00. 笔记盒/10-Literature/" },
  { name = "双反斜杠转义", path = "00. 笔记盒\\20-Permanent" },
  { name = "单引号字面量", path = '00. 笔记盒\30-Index' },
  { name = "冲突转义字母", path = "notes\new\temp\ref\foo\box\unicode\" },
  { name = "越界防护", path = "..\..\safe\sub" },
  { name = "根目录", path = "" },
]
"#;
        let cfg = parse_config_str(raw).expect("all path style variants must parse cleanly");
        assert_eq!(cfg.vault_dir, r"C:\Users\wangh\OneDrive\Notebook");
        assert_eq!(
            cfg.resolve_attachments_dir(),
            PathBuf::from(r"C:\Users\wangh\OneDrive\Notebook")
                .join("00. 笔记盒")
                .join("attachments")
                .join("images")
        );

        let root = PathBuf::from(r"C:\Users\wangh\OneDrive\Notebook");
        assert_eq!(
            cfg.resolve_target_dir(0).1,
            root.join("00. 笔记盒").join("00-Inbox")
        );
        assert_eq!(
            cfg.resolve_target_dir(1).1,
            root.join("00. 笔记盒").join("10-Literature")
        );
        assert_eq!(
            cfg.resolve_target_dir(2).1,
            root.join("00. 笔记盒").join("20-Permanent")
        );
        assert_eq!(
            cfg.resolve_target_dir(3).1,
            root.join("00. 笔记盒").join("30-Index")
        );
        assert_eq!(
            cfg.resolve_target_dir(4).1,
            root.join("notes")
                .join("new")
                .join("temp")
                .join("ref")
                .join("foo")
                .join("box")
                .join("unicode")
        );
        assert_eq!(cfg.resolve_target_dir(5).1, root.join("safe").join("sub"));
        assert_eq!(cfg.resolve_target_dir(6).1, root);
    }

    #[test]
    fn array_of_tables_targets_and_comments_work_cleanly() {
        let raw = r#"
# path = "注释里的路径\00不应影响"
vault_dir = 'D:\Vault'

[[targets]]
name = "卡片"
path = "00. 笔记盒\00-Inbox"

[[targets]]
path = "aaa\bbb\ccc"
name = "深层目录"
"#;
        let cfg = parse_config_str(raw).expect("[[targets]] syntax must parse cleanly");
        assert_eq!(cfg.targets.len(), 2);
        assert_eq!(
            cfg.resolve_target_dir(0).1,
            PathBuf::from(r"D:\Vault").join("00. 笔记盒").join("00-Inbox")
        );
        assert_eq!(
            cfg.resolve_target_dir(1).1,
            PathBuf::from(r"D:\Vault").join("aaa").join("bbb").join("ccc")
        );
    }
}
