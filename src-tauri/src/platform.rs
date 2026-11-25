use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicIsize, Ordering};
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};

const RUN_REG_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "ZettelQuick";

/// 记录呼出笔记窗之前的上一个外部前台窗口句柄，以便取消置顶退至后方时无缝归还键盘焦点
static LAST_EXTERNAL_HWND: AtomicIsize = AtomicIsize::new(0);

/// 为无边框窗口启用 Windows 11 DWM 原生圆角与硬件阴影（Windows 10 下自动安全降级）
#[cfg(windows)]
pub fn apply_window_chrome(window: &WebviewWindow) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    if let Ok(hwnd) = window.hwnd() {
        let raw_hwnd = hwnd.0 as HWND;
        let corner_pref: u32 = DWMWCP_ROUND as u32;
        unsafe {
            let hr = DwmSetWindowAttribute(
                raw_hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &corner_pref as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            );
            crate::log_debug!(
                "PLATFORM",
                "Applied Win11 DWM rounded corner attribute (HRESULT=0x{:08x})",
                hr as u32
            );
        }
    }
}

#[cfg(not(windows))]
pub fn apply_window_chrome(_window: &WebviewWindow) {}

/// 将窗口定位到当前鼠标指针所在显示器的水平居中、垂直偏上 22% 位置
pub fn position_on_cursor_monitor(window: &WebviewWindow, logical_w: u32, logical_h: u32) {
    let cursor_opt = window.cursor_position().ok();
    let monitors = window.available_monitors().unwrap_or_default();

    let active_monitor = cursor_opt
        .and_then(|cursor| {
            monitors.iter().find(|m| {
                let pos = m.position();
                let size = m.size();
                cursor.x >= pos.x as f64
                    && cursor.x < (pos.x + size.width as i32) as f64
                    && cursor.y >= pos.y as f64
                    && cursor.y < (pos.y + size.height as i32) as f64
            })
        })
        .cloned()
        .or_else(|| window.primary_monitor().ok().flatten())
        .or_else(|| monitors.into_iter().next());

    let Some(monitor) = active_monitor else {
        crate::log_warn!("WINDOW", "Unable to detect any monitor when positioning window");
        return;
    };

    let scale = monitor.scale_factor();
    let phys_w = ((logical_w as f64) * scale).round() as u32;
    let phys_h = ((logical_h as f64) * scale).round() as u32;
    if let Err(e) = window.set_size(PhysicalSize::new(phys_w, phys_h)) {
        crate::log_warn!("WINDOW", "set_size({phys_w}x{phys_h}) failed: {e}");
    }

    let work_area = monitor.work_area();
    let area_x = work_area.position.x;
    let area_y = work_area.position.y;
    let area_w = work_area.size.width as i32;
    let area_h = work_area.size.height as i32;

    let target_x = area_x + ((area_w - phys_w as i32) / 2).max(0);
    let target_y = area_y + (((area_h - phys_h as i32).max(0) as f64) * 0.22).round() as i32;

    if let Err(e) = window.set_position(PhysicalPosition::new(target_x, target_y)) {
        crate::log_warn!("WINDOW", "set_position({target_x}, {target_y}) failed: {e}");
    }
    crate::log_debug!(
        "WINDOW",
        "Positioned window on monitor {:?}: cursor={:?}, scale={scale:.2}, target=({target_x}, {target_y}), phys_size={phys_w}x{phys_h}",
        monitor.name(),
        cursor_opt
    );
}

/// 强制将窗口固定在所有普通窗口最外层（HWND_TOPMOST）并赋予内部 WebView2 键盘输入焦点
pub fn show_topmost_and_force_focus(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_always_on_top(true);

    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
            SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
            SW_SHOW,
        };

        let target_hwnd = hwnd.0 as HWND;
        unsafe {
            let fg_hwnd = GetForegroundWindow();
            if !fg_hwnd.is_null() && fg_hwnd != target_hwnd {
                LAST_EXTERNAL_HWND.store(fg_hwnd as isize, Ordering::Relaxed);
            }

            let fg_thread = if !fg_hwnd.is_null() {
                GetWindowThreadProcessId(fg_hwnd, std::ptr::null_mut())
            } else {
                0
            };
            let cur_thread = GetCurrentThreadId();
            let attached = if fg_thread != 0 && fg_thread != cur_thread {
                AttachThreadInput(fg_thread, cur_thread, 1) != 0
            } else {
                false
            };

            ShowWindow(target_hwnd, SW_SHOW);
            let _ = SetWindowPos(
                target_hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = BringWindowToTop(target_hwnd);
            let _ = SetForegroundWindow(target_hwnd);

            if attached {
                let _ = AttachThreadInput(fg_thread, cur_thread, 0);
            }
        }
    }

    // 关键顺序：必须在外层 HWND 成为系统前台窗口之后，再调用 Tauri set_focus()
    // 将键盘焦点下发给内部 WebView2 控件，否则外层容器会吞掉首次按键（如 Esc）
    let _ = window.set_focus();
}

/// 仅修改窗口置顶层级（绝不附带 SWP_SHOWWINDOW，避免在后台静默启动时误把隐藏窗口画上屏幕）
pub fn promote_window_topmost(window: &WebviewWindow) {
    let _ = window.set_always_on_top(true);
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        };
        let target_hwnd = hwnd.0 as HWND;
        unsafe {
            let _ = SetWindowPos(
                target_hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    let _ = window.set_focus();
}

/// 取消窗口最外层置顶，立即将其压到其他窗口后面（HWND_BOTTOM），并将输入焦点归还给先前的外部窗口
pub fn send_window_to_back(window: &WebviewWindow) {
    let _ = window.set_always_on_top(false);

    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::Foundation::{HWND, POINT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetCursorPos, IsWindow, IsWindowVisible, SetForegroundWindow,
            SetWindowPos, WindowFromPoint, GA_ROOT, HWND_BOTTOM, HWND_NOTOPMOST, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE,
        };

        let target_hwnd = hwnd.0 as HWND;
        unsafe {
            // 先解除 TOPMOST 扩展样式，再压入 Z 序底层
            let _ = SetWindowPos(
                target_hwnd,
                HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            let _ = SetWindowPos(
                target_hwnd,
                HWND_BOTTOM,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );

            // 将键盘焦点交还给呼出前的外部活动窗口（若已关闭则尝试激活鼠标下方的顶层窗口）
            let prev_raw = LAST_EXTERNAL_HWND.load(Ordering::Relaxed);
            let mut restored_focus = false;
            if prev_raw != 0 {
                let prev_hwnd = prev_raw as HWND;
                if prev_hwnd != target_hwnd
                    && IsWindow(prev_hwnd) != 0
                    && IsWindowVisible(prev_hwnd) != 0
                {
                    restored_focus = SetForegroundWindow(prev_hwnd) != 0;
                }
            }

            if !restored_focus {
                let mut pt = POINT { x: 0, y: 0 };
                if GetCursorPos(&mut pt) != 0 {
                    let under_cursor = WindowFromPoint(pt);
                    if !under_cursor.is_null() {
                        let root_hwnd = GetAncestor(under_cursor, GA_ROOT);
                        if !root_hwnd.is_null() && root_hwnd != target_hwnd {
                            let _ = SetForegroundWindow(root_hwnd);
                        }
                    }
                }
            }
        }
    }
    crate::log_info!(
        "WINDOW",
        "Demoted capture window to HWND_BOTTOM behind other windows"
    );
}

#[cfg(windows)]
fn build_autostart_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --autostart", exe.to_string_lossy()))
}

#[cfg(windows)]
pub fn is_autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run_key) = hkcu.open_subkey(RUN_REG_KEY) else {
        return false;
    };
    run_key.get_value::<String, _>(RUN_VALUE_NAME).is_ok()
}

#[cfg(not(windows))]
pub fn is_autostart_enabled() -> bool {
    false
}

#[cfg(windows)]
pub fn set_autostart_enabled(enabled: bool) -> Result<bool, String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu
        .create_subkey(RUN_REG_KEY)
        .map_err(|e| format!("打开注册表启动项失败: {e}"))?;

    if enabled {
        let cmd = build_autostart_command().ok_or_else(|| "无法获取当前可执行文件路径".to_string())?;
        run_key
            .set_value(RUN_VALUE_NAME, &cmd)
            .map_err(|e| {
                crate::log_error!("PLATFORM", "Failed writing HKCU Run autostart registry: {e}");
                format!("写入开机自启注册表失败: {e}")
            })?;
        crate::log_info!("PLATFORM", "Autostart enabled in HKCU Run -> {cmd}");
        Ok(true)
    } else {
        let _ = run_key.delete_value(RUN_VALUE_NAME);
        crate::log_info!("PLATFORM", "Autostart disabled (removed from HKCU Run)");
        Ok(false)
    }
}

#[cfg(not(windows))]
pub fn set_autostart_enabled(_enabled: bool) -> Result<bool, String> {
    Ok(false)
}

/// 若用户此前已开启开机自启，并将原便携 .exe 移动/重命名到了新目录（原路径已不存在），启动时自动修复注册表路径
#[cfg(windows)]
pub fn heal_autostart_path_if_enabled() {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(run_key) = hkcu.open_subkey(RUN_REG_KEY) else {
        return;
    };
    let Ok(raw_cmd) = run_key.get_value::<String, _>(RUN_VALUE_NAME) else {
        return;
    };

    // 提取已注册命令中的可执行文件路径：仅当旧路径已不存在时才自动迁移更新，避免临时或开发目录副本覆盖正式安装路径
    let extracted_exe = if let Some(rest) = raw_cmd.trim().strip_prefix('"') {
        rest.split_once('"').map(|(exe, _)| exe)
    } else {
        raw_cmd.trim().split_whitespace().next()
    };

    if let Some(old_exe) = extracted_exe {
        if !Path::new(old_exe).exists() {
            let _ = set_autostart_enabled(true);
        }
    }
}

#[cfg(not(windows))]
pub fn heal_autostart_path_if_enabled() {}

/// 使用 Windows 资源管理器或系统默认关联程序打开指定目录或文件
pub fn open_in_os(path: &Path) {
    crate::log_info!("PLATFORM", "Opening path in OS shell: '{}'", path.display());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let spawn_res = if path.is_dir() {
            Command::new("explorer").arg(path).spawn()
        } else {
            Command::new("cmd")
                .args(["/C", "start", "", &path.to_string_lossy()])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
        };
        if let Err(e) = spawn_res {
            crate::log_error!(
                "PLATFORM",
                "Failed opening path '{}' in OS shell: {e}",
                path.display()
            );
        }
    }
}
