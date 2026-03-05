#[cfg(target_os = "windows")]
mod win_launcher {
    use std::collections::HashSet;
    use std::mem::size_of;
    use std::path::Path;
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindow, IsWindowVisible, SetWindowPos, ShowWindow, SM_CXSCREEN, SM_CYSCREEN,
        SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_RESTORE,
    };

    const WOW_EXE: &str = "Wow.exe";
    const WOW_ARGS: &[&str] = &["-windowed"];
    const WOW_PROCESS_NAMES: &[&str] = &[
        "wow.exe",
        "wow-64.exe",
        "wowclassic.exe",
        "wowclassict.exe",
        "wowclasst.exe",
        "wowb.exe",
    ];
    const TARGET_WIDTH: i32 = 500;
    const TARGET_HEIGHT: i32 = 500;
    const BOTTOM_MARGIN: i32 = 40;
    const WAIT_TIMEOUT: Duration = Duration::from_secs(60);
    const WAIT_AFTER_LAUNCH: Duration = Duration::from_millis(0);
    const ENFORCE_DURATION: Duration = Duration::from_secs(60);
    const ENFORCE_INTERVAL: Duration = Duration::from_millis(200);

    #[derive(Clone)]
    struct ProcessInfo {
        pid: u32,
        parent_pid: u32,
        exe_name: String,
    }

    struct SearchContext {
        pids: HashSet<u32>,
        title_hints: Vec<String>,
        found: Vec<HWND>,
    }

    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&ch| ch == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn collect_processes() -> Vec<ProcessInfo> {
        let mut processes = Vec::new();

        unsafe {
            let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
                Ok(handle) => handle,
                Err(_) => return processes,
            };

            let mut entry = PROCESSENTRY32W::default();
            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    processes.push(ProcessInfo {
                        pid: entry.th32ProcessID,
                        parent_pid: entry.th32ParentProcessID,
                        exe_name: wide_to_string(&entry.szExeFile),
                    });

                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }

            close_handle(snapshot);
        }

        processes
    }

    fn close_handle(handle: HANDLE) {
        unsafe {
            let _ = CloseHandle(handle);
        }
    }

    fn read_title_hints() -> Vec<String> {
        let raw = std::env::var("WOW_WINDOW_TITLES")
            .unwrap_or_else(|_| "World of Warcraft".to_string());
        raw.split(',')
            .map(|part| part.trim().to_lowercase())
            .filter(|part| !part.is_empty())
            .collect()
    }

    fn process_exists(pid: u32) -> bool {
        collect_processes().iter().any(|proc| proc.pid == pid)
    }

    fn collect_candidate_pids(
        launched_pid: u32,
        launcher_exit_pid: Option<u32>,
        before_launch: &HashSet<u32>,
    ) -> Vec<u32> {
        let processes = collect_processes();
        let mut candidates = HashSet::new();
        candidates.insert(launched_pid);

        if let Some(pid) = launcher_exit_pid {
            candidates.insert(pid);
        }

        // 覆盖“启动器进程 -> 实际游戏进程”的场景：抓取新建子进程链。
        let mut changed = true;
        while changed {
            changed = false;
            for proc in &processes {
                if before_launch.contains(&proc.pid) {
                    continue;
                }
                if candidates.contains(&proc.parent_pid) && candidates.insert(proc.pid) {
                    changed = true;
                }
            }
        }

        // 同时兼容常见 WoW 进程名，避免只盯住一个可执行名。
        for proc in &processes {
            if before_launch.contains(&proc.pid) {
                continue;
            }
            if WOW_PROCESS_NAMES
                .iter()
                .any(|name| proc.exe_name.eq_ignore_ascii_case(name))
            {
                candidates.insert(proc.pid);
            }
        }

        let mut candidates: Vec<u32> = candidates.into_iter().collect();
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }

    unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let context = &mut *(lparam.0 as *mut SearchContext);

        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut matched = context.pids.contains(&pid);

        if !matched && !context.title_hints.is_empty() {
            let title_len = GetWindowTextLengthW(hwnd);
            if title_len > 0 {
                let mut title_buf = vec![0u16; (title_len + 1) as usize];
                let copied = GetWindowTextW(hwnd, &mut title_buf);
                if copied > 0 {
                    let title = String::from_utf16_lossy(&title_buf[..copied as usize]).to_lowercase();
                    matched = context
                        .title_hints
                        .iter()
                        .any(|hint| title.contains(hint));
                }
            }
        }

        if !matched {
            return BOOL(1);
        }

        context.found.push(hwnd);
        BOOL(1)
    }

    fn find_windows(pids: &[u32], title_hints: &[String]) -> Vec<HWND> {
        if pids.is_empty() && title_hints.is_empty() {
            return Vec::new();
        }

        let mut context = SearchContext {
            pids: pids.iter().copied().collect(),
            title_hints: title_hints.to_vec(),
            found: Vec::new(),
        };

        unsafe {
            let _ = EnumWindows(
                Some(enum_windows_callback),
                LPARAM((&mut context as *mut SearchContext) as isize),
            );
        }

        context.found
    }

    fn apply_window_layout(hwnd: HWND) -> bool {
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return false;
            }

            let _ = ShowWindow(hwnd, SW_RESTORE);

            let screen_width = GetSystemMetrics(SM_CXSCREEN);
            let screen_height = GetSystemMetrics(SM_CYSCREEN);
            let pos_x = screen_width - TARGET_WIDTH;
            let pos_y = screen_height - TARGET_HEIGHT - BOTTOM_MARGIN;

            SetWindowPos(
                hwnd,
                None,
                pos_x,
                pos_y,
                TARGET_WIDTH,
                TARGET_HEIGHT,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .is_ok()
        }
    }

    pub fn run() {
        if !Path::new(WOW_EXE).exists() {
            eprintln!("未找到 {}，请把启动器和 wow.exe 放在同一目录", WOW_EXE);
            std::process::exit(1);
        }

        let before_launch: HashSet<u32> = collect_processes().into_iter().map(|p| p.pid).collect();

        let mut child = match Command::new(WOW_EXE).args(WOW_ARGS).spawn() {
            Ok(child) => child,
            Err(err) => {
                eprintln!("启动 {} 失败: {}", WOW_EXE, err);
                std::process::exit(1);
            }
        };

        let launched_pid = child.id();
        let start = Instant::now();
        let mut launcher_exit_pid: Option<u32> = None;
        let title_hints = read_title_hints();

        println!(
            "已启动 WoW，等待窗口并强制固定到右下角小窗... (标题关键字: {:?})",
            title_hints
        );

        let first_layout_ok = loop {
            if launcher_exit_pid.is_none() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if let Some(code) = status.code() {
                            if code > 0 {
                                let pid = code as u32;
                                if process_exists(pid) {
                                    launcher_exit_pid = Some(pid);
                                }
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {}
                }
            }

            if start.elapsed() >= WAIT_AFTER_LAUNCH {
                let pids = collect_candidate_pids(launched_pid, launcher_exit_pid, &before_launch);
                let windows = find_windows(&pids, &title_hints);

                let mut moved = false;
                for hwnd in windows {
                    if apply_window_layout(hwnd) {
                        moved = true;
                    }
                }

                if moved {
                    break true;
                }
            }

            if start.elapsed() > WAIT_TIMEOUT {
                break false;
            }

            sleep(ENFORCE_INTERVAL);
        };

        if !first_layout_ok {
            eprintln!("超时：未能找到并固定 WoW 主窗口，请确认是窗口模式启动。");
            std::process::exit(1);
        }

        let enforce_start = Instant::now();
        while enforce_start.elapsed() < ENFORCE_DURATION {
            let pids = collect_candidate_pids(launched_pid, launcher_exit_pid, &before_launch);
            let windows = find_windows(&pids, &title_hints);
            for hwnd in windows {
                let _ = apply_window_layout(hwnd);
            }
            sleep(ENFORCE_INTERVAL);
        }

        println!(
            "已将 WoW 固定为右下角小窗（{}x{}）",
            TARGET_WIDTH,
            TARGET_HEIGHT
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
