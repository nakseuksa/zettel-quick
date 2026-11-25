#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    zettel_quick_lib::logger::init_early_logger();

    // 将 Edge WebView2 运行缓存重定向到 %LOCALAPPDATA%\ZettelQuick\WebView2，
    // 确保绿色便携目录仅保留干净的 zettel-quick.exe 与 config.toml
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none() {
        let webview_dir = zettel_quick_lib::config::webview_data_dir();
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &webview_dir);
    }

    let exe_path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let args: Vec<String> = std::env::args().collect();

    zettel_quick_lib::log_info!(
        "BOOT",
        "========== ZettelQuick v{} starting (pid={}, exe='{}', log='{}', args={:?}) ==========",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        exe_path,
        zettel_quick_lib::logger::active_log_file_path().display(),
        args
    );

    zettel_quick_lib::run();
}
