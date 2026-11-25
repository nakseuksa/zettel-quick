#[macro_use]
pub mod logger;
pub mod config;
pub mod draft;
pub mod indexer;
pub mod platform;
pub mod writer;

use config::{config_file_path, draft_cache_path, load_or_init_config, parse_config_str, AppConfig};
use draft::DraftSession;
use indexer::{CardSuggestion, TagSuggestion, VaultIndexer};
use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use writer::{archive_session, paste_clipboard_image_to_vault, ArchiveOutcome};

pub struct AppState {
    pub config: RwLock<AppConfig>,
    pub draft: Mutex<DraftSession>,
    pub indexer: VaultIndexer,
    pub pinned: AtomicBool,
    pub config_watcher: Mutex<Option<RecommendedWatcher>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendBootState {
    pub config: AppConfig,
    pub draft_content: String,
    pub target_index: usize,
    pub is_recalling_last: bool,
    pub has_last_archived: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowActivatedPayload {
    pub has_last_archived: bool,
    pub is_recalling_last: bool,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveResponse {
    pub outcome: ArchiveOutcome,
    pub has_last_archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscardResponse {
    pub has_last_archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallResponse {
    pub content: String,
    pub target_index: usize,
    pub filename: String,
}

#[tauri::command]
fn get_initial_state(state: State<'_, Arc<AppState>>) -> FrontendBootState {
    let cfg = state.config.read().clone();
    let draft = state.draft.lock();
    FrontendBootState {
        config: cfg,
        draft_content: draft.content.clone(),
        target_index: draft.target_index,
        is_recalling_last: draft.active_recall.is_some(),
        has_last_archived: draft.last_archived.is_some(),
        pinned: state.pinned.load(Ordering::Relaxed),
    }
}

#[tauri::command]
fn sync_draft(state: State<'_, Arc<AppState>>, content: String, target_index: usize) {
    let mut draft = state.draft.lock();
    draft.update_content(content, target_index);
}

#[tauri::command]
fn archive_card(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    content: String,
    target_index: usize,
    hide_after: bool,
) -> Result<ArchiveResponse, String> {
    let cfg = state.config.read().clone();
    let prev_recalled_path = {
        let draft = state.draft.lock();
        draft.active_recall.as_ref().map(|r| r.file_path.clone())
    };

    let (outcome, has_last_archived) = {
        let mut draft = state.draft.lock();
        let res = archive_session(&cfg, &mut draft, content, target_index)?;
        (res, draft.last_archived.is_some())
    };

    // 同步增量索引，确保新落盘或改名的卡片 0ms 可在下一张卡 [[ 补全中检索到
    match &outcome {
        ArchiveOutcome::Saved { file_path, .. } => {
            let new_p = Path::new(file_path);
            if let Some(old_p) = prev_recalled_path {
                if old_p != new_p {
                    state.indexer.remove_single_file(&old_p);
                }
            }
            state.indexer.upsert_single_file(new_p);
        }
        ArchiveOutcome::DeletedRecalled { deleted_path } => {
            state.indexer.remove_single_file(Path::new(deleted_path));
        }
        ArchiveOutcome::EmptySkipped => {}
    }

    if hide_after {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }

    Ok(ArchiveResponse {
        outcome,
        has_last_archived,
    })
}

#[tauri::command]
fn discard_draft(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    hide_after: bool,
) -> Result<DiscardResponse, String> {
    let has_last_archived = {
        let mut draft = state.draft.lock();
        draft.discard_current_draft();
        draft.last_archived.is_some()
    };

    if hide_after {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.hide();
        }
    }

    Ok(DiscardResponse { has_last_archived })
}

#[tauri::command]
fn recall_last_card(state: State<'_, Arc<AppState>>) -> Result<RecallResponse, String> {
    let mut draft = state.draft.lock();
    let recalled = draft.begin_recall_last()?;
    let filename = recalled
        .file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(RecallResponse {
        content: recalled.raw_body,
        target_index: recalled.target_index,
        filename,
    })
}

#[tauri::command]
fn paste_clipboard_image(state: State<'_, Arc<AppState>>) -> Result<Option<String>, String> {
    let cfg = state.config.read().clone();
    let mut draft = state.draft.lock();
    paste_clipboard_image_to_vault(&cfg, &mut draft)
}

#[tauri::command]
fn search_cards(state: State<'_, Arc<AppState>>, query: String, limit: usize) -> Vec<CardSuggestion> {
    state.indexer.search_cards(&query, limit)
}

#[tauri::command]
fn search_tags(state: State<'_, Arc<AppState>>, query: String, limit: usize) -> Vec<TagSuggestion> {
    state.indexer.search_tags(&query, limit)
}

#[tauri::command]
fn toggle_pin(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let next = !state.pinned.load(Ordering::Relaxed);
    state.pinned.store(next, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("main") {
        if next {
            platform::promote_window_topmost(&win);
            log_info!("WINDOW", "Ctrl+Shift+P / Pin toggled -> promoted to TOPMOST (pinned=true)");
        } else {
            platform::send_window_to_back(&win);
            log_info!(
                "WINDOW",
                "Ctrl+Shift+P / Pin toggled -> demoted behind other windows (pinned=false)"
            );
        }
    }
    Ok(next)
}

#[tauri::command]
fn hide_capture_window(app: AppHandle) {
    log_debug!("WINDOW", "Frontend requested hide_capture_window");
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
}

#[tauri::command]
fn log_frontend_event(level: String, scope: String, message: String) {
    let lvl = logger::LogLevel::from_str_lossy(&level);
    let target = if scope.trim().is_empty() {
        "FRONTEND"
    } else {
        scope.trim()
    };
    logger::write_log(lvl, target, &message);
}

/// 前端 DOM 与键盘事件挂载完毕后回调：若当前窗口已处于显示态（如首次引导后立即弹出），补发一次内部焦点绑定
#[tauri::command]
fn notify_frontend_ready(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_visible().unwrap_or(false) {
            log_debug!(
                "WINDOW",
                "Frontend ready while window is visible -> reinforcing WebView2 focus"
            );
            platform::show_topmost_and_force_focus(&win);
        }
    }
}

/// 唤起编辑弹窗：强制恢复固定在所有窗口最外层（HWND_TOPMOST），若原本处于隐藏态则定位到当前鼠标所在屏幕
pub fn summon_window(app: &AppHandle, state: &Arc<AppState>) {
    let Some(win) = app.get_webview_window("main") else {
        log_error!("WINDOW", "summon_window failed: WebviewWindow 'main' not found");
        return;
    };

    let is_visible = win.is_visible().unwrap_or(false);
    // 每次主动唤起笔记窗时，自动固定在最外层保证不被其他窗口遮挡
    state.pinned.store(true, Ordering::Relaxed);

    if !is_visible {
        let cfg = state.config.read();
        platform::position_on_cursor_monitor(&win, cfg.ui.width, cfg.ui.height);
    }

    platform::show_topmost_and_force_focus(&win);

    let payload = {
        let draft = state.draft.lock();
        WindowActivatedPayload {
            has_last_archived: draft.last_archived.is_some(),
            is_recalling_last: draft.active_recall.is_some(),
            pinned: true,
        }
    };
    log_info!(
        "WINDOW",
        "Summoned capture window as TOPMOST (was_visible={is_visible}, recalling={})",
        payload.is_recalling_last
    );
    let _ = app.emit("app:window-activated", payload);
}

/// 全局热键处理：若当前可见、已固定在最外层且拥有输入焦点 -> 隐藏暂存草稿；否则 -> 唤起并固定到最外层聚焦
pub fn toggle_capture_window(app: &AppHandle, state: &Arc<AppState>) {
    let Some(win) = app.get_webview_window("main") else {
        log_error!("WINDOW", "toggle_capture_window: WebviewWindow 'main' missing");
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    let focused = win.is_focused().unwrap_or(false);
    let is_pinned = state.pinned.load(Ordering::Relaxed);

    if visible && is_pinned && focused {
        let draft_chars = state.draft.lock().content.chars().count();
        log_info!(
            "WINDOW",
            "Toggle -> hiding active topmost window (preserving draft chars={draft_chars})"
        );
        let _ = win.hide();
    } else {
        log_info!(
            "WINDOW",
            "Toggle -> summoning to topmost (prev visible={visible}, pinned={is_pinned}, focused={focused})"
        );
        summon_window(app, state);
    }
}

fn register_hotkey_from_config(app: &AppHandle, shortcut_str: &str) -> Result<(), String> {
    let trimmed = shortcut_str.trim();
    if trimmed.is_empty() {
        log_warn!("HOTKEY", "Global shortcut string is empty -> skipping registration");
        return Err("快捷键配置为空".to_string());
    }
    let parsed: Shortcut = trimmed.parse().map_err(|e| {
        let msg = format!("无法解析快捷键 '{trimmed}': {e:?}");
        log_error!("HOTKEY", "{msg}");
        msg
    })?;

    let _ = app.global_shortcut().unregister_all();
    match app.global_shortcut().register(parsed) {
        Ok(_) => {
            log_info!("HOTKEY", "Registered global shortcut '{trimmed}'");
            Ok(())
        }
        Err(e) => {
            let msg = format!("注册全局快捷键 '{trimmed}' 失败（可能被其他软件占用）: {e}");
            log_error!("HOTKEY", "{msg}");
            Err(msg)
        }
    }
}

fn setup_tray(app: &AppHandle, state: Arc<AppState>) -> tauri::Result<()> {
    let hk = state.config.read().hotkey.toggle.clone();
    let toggle_label = format!("呼出 / 隐藏卡片窗 ({hk})");

    let item_toggle = MenuItem::with_id(app, "toggle", &toggle_label, true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let item_open_vault = MenuItem::with_id(app, "open_vault", "打开卡片盒目录", true, None::<&str>)?;
    let item_edit_cfg =
        MenuItem::with_id(app, "edit_config", "编辑配置文件 (config.toml)", true, None::<&str>)?;
    let item_open_log =
        MenuItem::with_id(app, "open_log", "查看运行日志 (zettel-quick.log)", true, None::<&str>)?;
    let item_reindex = MenuItem::with_id(app, "reindex", "重新扫描全库索引", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let item_autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "开机自动启动",
        true,
        platform::is_autostart_enabled(),
        None::<&str>,
    )?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let item_quit = MenuItem::with_id(app, "quit", "退出 ZettelQuick", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &item_toggle,
            &sep1,
            &item_open_vault,
            &item_edit_cfg,
            &item_open_log,
            &item_reindex,
            &sep2,
            &item_autostart,
            &sep3,
            &item_quit,
        ],
    )?;

    let autostart_ref = item_autostart.clone();
    let state_menu = Arc::clone(&state);
    let state_click = Arc::clone(&state);

    let mut tray_builder = TrayIconBuilder::with_id("zettel-tray")
        .tooltip(&format!("ZettelQuick · 按 {hk} 速记卡片"))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app_handle, event| {
            let menu_id = event.id().as_ref();
            log_info!("TRAY", "Tray menu clicked: '{menu_id}'");
            match menu_id {
                "toggle" => toggle_capture_window(app_handle, &state_menu),
                "open_vault" => {
                    let vault = state_menu.config.read().vault_path();
                    let _ = fs::create_dir_all(&vault);
                    platform::open_in_os(&vault);
                }
                "edit_config" => {
                    let cfg_path = config_file_path();
                    platform::open_in_os(&cfg_path);
                }
                "open_log" => {
                    logger::flush_logs();
                    let log_path = logger::active_log_file_path();
                    platform::open_in_os(&log_path);
                }
                "reindex" => {
                    let cfg = state_menu.config.read().clone();
                    state_menu.indexer.rebuild_and_watch(&cfg);
                }
                "autostart" => {
                    let desired = !platform::is_autostart_enabled();
                    if let Ok(actual) = platform::set_autostart_enabled(desired) {
                        let _ = autostart_ref.set_checked(actual);
                    } else {
                        let _ = autostart_ref.set_checked(platform::is_autostart_enabled());
                    }
                }
                "quit" => {
                    log_info!("APP", "User clicked Quit -> persisting draft snapshot and exiting");
                    state_menu.draft.lock().persist_snapshot();
                    logger::flush_logs();
                    app_handle.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                log_debug!("TRAY", "Tray left-click -> toggling capture window");
                toggle_capture_window(tray.app_handle(), &state_click);
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    }

    tray_builder.build(app)?;
    Ok(())
}

/// 监听同级 config.toml 保存事件，实时热重载快捷键、目录索引与界面配置
fn start_config_hot_reload_watcher(app: AppHandle, state: Arc<AppState>) {
    let cfg_path = config_file_path();
    let Some(parent_dir) = cfg_path.parent().map(|p| p.to_path_buf()) else {
        return;
    };
    let target_file_name = cfg_path.file_name().map(|n| n.to_os_string());

    let state_for_cb = Arc::clone(&state);
    let watcher_res = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                return;
            }
            let hit = event.paths.iter().any(|p| p.file_name() == target_file_name.as_deref());
            if !hit {
                return;
            }

            let Ok(raw) = fs::read_to_string(&cfg_path) else {
                log_warn!("CONFIG", "Hot-reload: failed reading '{}'", cfg_path.display());
                return;
            };
            let new_cfg = match parse_config_str(&raw) {
                Ok(c) => c,
                Err(err) => {
                    // 用户编辑到一半语法暂未闭合时保持原配置运行，但写入明确日志方便排错
                    log_warn!(
                        "CONFIG",
                        "Hot-reload skipped due to invalid config.toml syntax (keeping previous config): {err}"
                    );
                    return;
                }
            };

            let prev_cfg = state_for_cb.config.read().clone();
            if new_cfg == prev_cfg {
                return;
            }

            if new_cfg.logging != prev_cfg.logging {
                logger::apply_logging_config(&new_cfg.logging);
                log_info!(
                    "CONFIG",
                    "Hot-reloaded logging config: enabled={}, level='{}'",
                    new_cfg.logging.enabled,
                    new_cfg.logging.level
                );
            }

            if new_cfg.hotkey.toggle != prev_cfg.hotkey.toggle {
                log_info!(
                    "CONFIG",
                    "Hot-reloading global hotkey: '{}' -> '{}'",
                    prev_cfg.hotkey.toggle,
                    new_cfg.hotkey.toggle
                );
                let _ = register_hotkey_from_config(&app, &new_cfg.hotkey.toggle);
                if let Some(tray) = app.tray_by_id("zettel-tray") {
                    let _ = tray.set_tooltip(Some(&format!(
                        "ZettelQuick · 按 {} 速记卡片",
                        new_cfg.hotkey.toggle
                    )));
                }
            }

            let need_reindex = new_cfg.vault_dir != prev_cfg.vault_dir
                || new_cfg.ignore_dirs != prev_cfg.ignore_dirs
                || new_cfg.frontmatter.extra_tags != prev_cfg.frontmatter.extra_tags;

            *state_for_cb.config.write() = new_cfg.clone();

            if need_reindex {
                log_info!("CONFIG", "Vault or index rules changed -> triggering full reindex");
                state_for_cb.indexer.rebuild_and_watch(&new_cfg);
            } else {
                log_info!("CONFIG", "Hot-reloaded config.toml successfully");
            }

            let _ = app.emit("app:config-updated", &new_cfg);
        },
        NotifyConfig::default(),
    );

    if let Ok(mut watcher) = watcher_res {
        if watcher.watch(&parent_dir, RecursiveMode::NonRecursive).is_ok() {
            *state.config_watcher.lock() = Some(watcher);
        }
    }
}

fn configure_main_window(win: &WebviewWindow, _cfg: &AppConfig) {
    platform::apply_window_chrome(win);
    // 初始化阶段窗口尚处于隐藏态（visible: false），仅标记置顶属性，绝不触发显示或抢焦点
    let _ = win.set_always_on_top(true);
}

pub fn run() {
    platform::heal_autostart_path_if_enabled();

    let (initial_cfg, is_first_init) = load_or_init_config();
    logger::apply_logging_config(&initial_cfg.logging);

    let indexer = VaultIndexer::new();
    indexer.rebuild_and_watch(&initial_cfg);

    let shared_state = Arc::new(AppState {
        config: RwLock::new(initial_cfg.clone()),
        draft: Mutex::new(DraftSession::new(draft_cache_path())),
        indexer,
        pinned: AtomicBool::new(true),
        config_watcher: Mutex::new(None),
    });

    let state_for_single = Arc::clone(&shared_state);
    let state_for_hotkey = Arc::clone(&shared_state);
    let state_for_setup = Arc::clone(&shared_state);

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(move |app, args, _cwd| {
            // 再次双击 .exe 时直接唤醒已驻留实例的编辑窗
            log_info!(
                "APP",
                "Second instance launch intercepted (args={args:?}) -> summoning existing window"
            );
            summon_window(app, &state_for_single);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        log_debug!("HOTKEY", "Global shortcut pressed: {shortcut:?}");
                        toggle_capture_window(app, &state_for_hotkey);
                    }
                })
                .build(),
        )
        .manage(shared_state)
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Some(win) = app.get_webview_window("main") {
                configure_main_window(&win, &initial_cfg);

                let win_handle = handle.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        // 拦截 Alt+F4，转为归档当前卡片并隐藏回托盘，保持 WebView2 热驻留
                        log_info!(
                            "WINDOW",
                            "CloseRequested (Alt+F4) intercepted -> requesting archive and hide"
                        );
                        api.prevent_close();
                        let _ = win_handle.emit("app:request-archive-and-hide", ());
                    }
                });
            }

            setup_tray(&handle, Arc::clone(&state_for_setup))?;
            let _ = register_hotkey_from_config(&handle, &initial_cfg.hotkey.toggle);
            start_config_hot_reload_watcher(handle.clone(), Arc::clone(&state_for_setup));

            // 首次初始化完配置（或带 --show 参数启动）时主动弹出一次卡片窗作为就绪反馈
            let launched_via_autostart = std::env::args().any(|a| a == "--autostart");
            let force_show = std::env::args().any(|a| a == "--show");
            log_info!(
                "APP",
                "Tauri setup ready (is_first_init={is_first_init}, autostart={launched_via_autostart}, force_show={force_show})"
            );
            if (is_first_init && !launched_via_autostart) || force_show {
                summon_window(&handle, &state_for_setup);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_initial_state,
            sync_draft,
            archive_card,
            discard_draft,
            recall_last_card,
            paste_clipboard_image,
            search_cards,
            search_tags,
            toggle_pin,
            hide_capture_window,
            log_frontend_event,
            notify_frontend_ready,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            log_error!("APP", "Fatal error running Tauri application: {e}");
            logger::flush_logs();
            panic!("启动 ZettelQuick 失败: {e}");
        });
}
