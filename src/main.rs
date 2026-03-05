#[cfg(target_os = "windows")]
mod win_launcher {
    use std::path::Path;
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetSystemMetrics, GetWindowThreadProcessId, GetWindow, IsWindowVisible,
        SetWindowPos, ShowWindow, GW_OWNER, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE,
        SWP_NOZORDER, SW_RESTORE,
    };

    const WOW_EXE: &str = "Wow.exe";
    const TARGET_WIDTH: i32 = 500;
    const TARGET_HEIGHT: i32 = 500;
    const BOTTOM_MARGIN: i32 = 40;
    const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

    struct SearchContext {
        pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &mut *(lparam.0 as *mut SearchContext);

        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != context.pid {
            return BOOL(1);
        }

        // 仅接收无 owner 的顶层窗口，避免误命中临时/附属窗口。
        let owner = match GetWindow(hwnd, GW_OWNER) {
            Ok(owner) => owner,
            Err(_) => return BOOL(1),
        };

        if !owner.0.is_null() {
            return BOOL(1);
        }

        context.found = Some(hwnd);
        BOOL(0)
    }

    fn find_window_for_process(pid: u32) -> Option<HWND> {
        let mut context = SearchContext {
            pid,
            found: None,
        };

        unsafe {
            let _ = EnumWindows(
                Some(enum_windows_callback),
                LPARAM((&mut context as *mut SearchContext) as isize),
            );
        }

        context.found
    }

    pub fn run() {
        if !Path::new(WOW_EXE).exists() {
            eprintln!("未找到 {}，请把启动器和 wow.exe 放在同一目录", WOW_EXE);
            std::process::exit(1);
        }

        let child = match Command::new(WOW_EXE).spawn() {
            Ok(child) => child,
            Err(err) => {
                eprintln!("启动 {} 失败: {}", WOW_EXE, err);
                std::process::exit(1);
            }
        };

        let pid = child.id();
        let start = Instant::now();

        let hwnd = loop {
            if let Some(hwnd) = find_window_for_process(pid) {
                break hwnd;
            }

            if start.elapsed() > WAIT_TIMEOUT {
                eprintln!("没找到游戏窗口!");
                std::process::exit(1);
            }

            sleep(Duration::from_millis(200));
        };

        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);

            let screen_width = GetSystemMetrics(SM_CXSCREEN);
            let screen_height = GetSystemMetrics(SM_CYSCREEN);
            let pos_x = screen_width - TARGET_WIDTH;
            let pos_y = screen_height - TARGET_HEIGHT - BOTTOM_MARGIN;

            let moved = SetWindowPos(
                hwnd,
                None,
                pos_x,
                pos_y,
                TARGET_WIDTH,
                TARGET_HEIGHT,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            if moved.is_err() {
                eprintln!("窗口已找到，但移动/缩放失败");
                std::process::exit(1);
            }
        }

        println!(
            "已启动 WoW，并将窗口调整为 {}x{}，定位到屏幕右下角",
            TARGET_WIDTH, TARGET_HEIGHT
        );
    }
}

#[cfg(target_os = "windows")]
fn main() {
    win_launcher::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("该启动器仅支持 Windows 平台。");
    std::process::exit(1);
}
