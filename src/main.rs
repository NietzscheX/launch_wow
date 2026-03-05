#[cfg(target_os = "windows")]
mod win_launcher {
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;
    use std::fs;
    use std::mem::size_of;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetWindowPos,
        ShowWindow, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SW_RESTORE,
    };

    const CONFIG_FILE_NAME: &str = "launch_wow.toml";
    const DEFAULT_WOW_EXE: &str = "Wow.exe";
    const DEFAULT_WOW_ARGS: &[&str] = &["-windowed"];
    const WOW_PROCESS_NAMES: &[&str] = &[
        "wow.exe",
        "wow-64.exe",
        "wowclassic.exe",
        "wowclassict.exe",
        "wowclasst.exe",
        "wowb.exe",
    ];
    const DEFAULT_TITLE_HINTS: &[&str] = &["World of Warcraft"];
    const DEFAULT_CLASS_HINTS: &[&str] = &["GxWindowClass"];
    const DEFAULT_TARGET_WIDTH: i32 = 500;
    const DEFAULT_TARGET_HEIGHT: i32 = 500;
    const DEFAULT_BOTTOM_MARGIN: i32 = 40;
    const DEFAULT_WAIT_TIMEOUT_MS: u64 = 60_000;
    const DEFAULT_WAIT_AFTER_LAUNCH_MS: u64 = 0;
    const DEFAULT_ENFORCE_INTERVAL_MS: u64 = 200;
    const DEFAULT_STABLE_CONFIRMATIONS: u32 = 5;
    const DEFAULT_POSITION_TOLERANCE: i32 = 4;

    #[derive(Clone)]
    struct ProcessInfo {
        pid: u32,
        parent_pid: u32,
        exe_name: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(default)]
    struct TomlConfig {
        wow_exe: String,
        wow_args: Vec<String>,
        class_hints: Vec<String>,
        title_hints: Vec<String>,
        target_width: i32,
        target_height: i32,
        bottom_margin: i32,
        wait_timeout_ms: u64,
        wait_after_launch_ms: u64,
        enforce_interval_ms: u64,
        stable_confirmations: u32,
        position_tolerance: i32,
    }

    impl Default for TomlConfig {
        fn default() -> Self {
            Self {
                wow_exe: DEFAULT_WOW_EXE.to_string(),
                wow_args: DEFAULT_WOW_ARGS.iter().map(|arg| (*arg).to_string()).collect(),
                class_hints: DEFAULT_CLASS_HINTS.iter().map(|hint| (*hint).to_string()).collect(),
                title_hints: DEFAULT_TITLE_HINTS.iter().map(|hint| (*hint).to_string()).collect(),
                target_width: DEFAULT_TARGET_WIDTH,
                target_height: DEFAULT_TARGET_HEIGHT,
                bottom_margin: DEFAULT_BOTTOM_MARGIN,
                wait_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
                wait_after_launch_ms: DEFAULT_WAIT_AFTER_LAUNCH_MS,
                enforce_interval_ms: DEFAULT_ENFORCE_INTERVAL_MS,
                stable_confirmations: DEFAULT_STABLE_CONFIRMATIONS,
                position_tolerance: DEFAULT_POSITION_TOLERANCE,
            }
        }
    }

    #[derive(Clone)]
    struct Config {
        wow_exe: String,
        wow_args: Vec<String>,
        class_hints: Vec<String>,
        title_hints: Vec<String>,
        target_width: i32,
        target_height: i32,
        bottom_margin: i32,
        wait_timeout: Duration,
        wait_after_launch: Duration,
        enforce_interval: Duration,
        stable_confirmations: u32,
        position_tolerance: i32,
    }

    struct SearchContext {
        pids: HashSet<u32>,
        class_hints: Vec<String>,
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

    fn normalize_hints(mut raw: Vec<String>, defaults: &[&str]) -> Vec<String> {
        raw.retain(|part| !part.trim().is_empty());
        let mut normalized: Vec<String> = raw
            .into_iter()
            .map(|part| part.trim().to_lowercase())
            .filter(|part| !part.is_empty())
            .collect()
        ;
        if normalized.is_empty() {
            normalized = defaults.iter().map(|hint| hint.to_lowercase()).collect();
        }
        normalized
    }

    fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for path in paths {
            let key = path.to_string_lossy().to_lowercase();
            if seen.insert(key) {
                out.push(path);
            }
        }
        out
    }

    fn config_candidates() -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        // 1) 可选：显式路径覆盖
        if let Ok(raw) = std::env::var("LAUNCH_WOW_CONFIG") {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }

        // 2) exe 同目录（双击运行时最符合预期）
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                candidates.push(dir.join(CONFIG_FILE_NAME));
            }
        }

        // 3) 当前工作目录
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(CONFIG_FILE_NAME));
        }

        // 4) 最后兜底：相对路径
        candidates.push(PathBuf::from(CONFIG_FILE_NAME));

        dedup_paths(candidates)
    }

    fn find_existing_config() -> Option<PathBuf> {
        config_candidates().into_iter().find(|path| path.exists())
    }

    fn create_default_config() -> Result<PathBuf, String> {
        let default_content = default_config_template();
        let targets = config_candidates();
        let mut errors = Vec::new();

        for target in targets {
            if let Some(parent) = target.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    errors.push(format!("创建目录失败 {}: {}", parent.display(), err));
                    continue;
                }
            }

            match fs::write(&target, &default_content) {
                Ok(_) => return Ok(target),
                Err(err) => errors.push(format!("写入失败 {}: {}", target.display(), err)),
            }
        }

        Err(format!(
            "无法自动创建配置文件 launch_wow.toml。尝试结果:\n{}",
            errors.join("\n")
        ))
    }

    fn resolve_wow_exe_path(wow_exe: &str, config_path: &Path) -> PathBuf {
        let candidate = Path::new(wow_exe);
        if candidate.is_absolute() {
            return candidate.to_path_buf();
        }
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(candidate)
    }

    fn default_config_template() -> String {
        format!(
            "# launch_wow.toml\n\
             # 如果 wow_exe 不是绝对路径，会按“配置文件所在目录”解析。\n\
             wow_exe = \"{}\"\n\
             \n\
             # 启动参数，默认保持窗口模式。\n\
             wow_args = [\"{}\"]\n\
             \n\
             # 窗口类名关键字（不区分大小写）。\n\
             # 常见值: GxWindowClass\n\
             class_hints = [\"{}\"]\n\
             \n\
             # 窗口标题关键字（不区分大小写）。\n\
             # 默认: World of Warcraft\n\
             title_hints = [\"{}\"]\n\
             \n\
             # 目标窗口大小（像素）。\n\
             target_width = {}\n\
             target_height = {}\n\
             \n\
             # 距离屏幕底边的额外边距（用于避开任务栏）。\n\
             bottom_margin = {}\n\
             \n\
             # 等待超时（毫秒）。超时后启动器退出并报错。\n\
             wait_timeout_ms = {}\n\
             \n\
             # 启动后延迟多少毫秒再开始窗口修正。\n\
             wait_after_launch_ms = {}\n\
             \n\
             # 修正窗口的轮询间隔（毫秒）。\n\
             enforce_interval_ms = {}\n\
             \n\
             # 连续命中多少次“已在目标位置尺寸”后，视为完成并自动退出。\n\
             stable_confirmations = {}\n\
             \n\
             # 坐标/尺寸允许误差（像素），用于应对 DPI 或边框差异。\n\
             position_tolerance = {}\n",
            DEFAULT_WOW_EXE,
            DEFAULT_WOW_ARGS[0],
            DEFAULT_CLASS_HINTS[0],
            DEFAULT_TITLE_HINTS[0],
            DEFAULT_TARGET_WIDTH,
            DEFAULT_TARGET_HEIGHT,
            DEFAULT_BOTTOM_MARGIN,
            DEFAULT_WAIT_TIMEOUT_MS,
            DEFAULT_WAIT_AFTER_LAUNCH_MS,
            DEFAULT_ENFORCE_INTERVAL_MS,
            DEFAULT_STABLE_CONFIRMATIONS,
            DEFAULT_POSITION_TOLERANCE
        )
    }

    fn load_config() -> Result<(Config, PathBuf), String> {
        let config_path = if let Some(path) = find_existing_config() {
            path
        } else {
            let created = create_default_config()?;
            println!("未找到配置文件，已生成默认配置: {}", created.display());
            created
        };

        if !config_path.exists() {
            return Err(format!(
                "配置文件路径不存在: {}",
                config_path.display()
            ));
        }

        if fs::metadata(&config_path).is_err() {
            return Err(format!(
                "无法访问配置文件: {}",
                config_path.display()
            ));
        }

        if fs::read_to_string(&config_path).is_err() {
            // 某些场景下文件刚创建后读取失败，再尝试一次写默认模板并读取。
            if let Err(err) = fs::write(&config_path, default_config_template()) {
                return Err(format!(
                    "配置文件损坏且重写失败 ({}): {}",
                    config_path.display(),
                    err
                ));
            }
        }

        let config_text = fs::read_to_string(&config_path)
            .map_err(|err| format!("读取配置文件失败 ({}): {}", config_path.display(), err))?;
        let file_config: TomlConfig = toml::from_str(&config_text)
            .map_err(|err| format!("解析 TOML 配置失败 ({}): {}", config_path.display(), err))?;

        let wow_exe = if file_config.wow_exe.trim().is_empty() {
            DEFAULT_WOW_EXE.to_string()
        } else {
            file_config.wow_exe.trim().to_string()
        };
        let wow_args: Vec<String> = if file_config.wow_args.is_empty() {
            DEFAULT_WOW_ARGS.iter().map(|arg| (*arg).to_string()).collect()
        } else {
            file_config
                .wow_args
                .iter()
                .map(|arg| arg.trim().to_string())
                .filter(|arg| !arg.is_empty())
                .collect()
        };

        let config = Config {
            wow_exe,
            wow_args: if wow_args.is_empty() {
                DEFAULT_WOW_ARGS.iter().map(|arg| (*arg).to_string()).collect()
            } else {
                wow_args
            },
            class_hints: normalize_hints(file_config.class_hints, DEFAULT_CLASS_HINTS),
            title_hints: normalize_hints(file_config.title_hints, DEFAULT_TITLE_HINTS),
            target_width: file_config.target_width,
            target_height: file_config.target_height,
            bottom_margin: file_config.bottom_margin,
            wait_timeout: Duration::from_millis(file_config.wait_timeout_ms),
            wait_after_launch: Duration::from_millis(file_config.wait_after_launch_ms),
            enforce_interval: Duration::from_millis(file_config.enforce_interval_ms),
            stable_confirmations: file_config.stable_confirmations,
            position_tolerance: file_config.position_tolerance,
        };

        if config.target_width <= 0 {
            return Err("-- target_width 必须大于 0".to_string());
        }
        if config.target_height <= 0 {
            return Err("-- target_height 必须大于 0".to_string());
        }
        if config.stable_confirmations == 0 {
            return Err("-- stable_confirmations 必须大于 0".to_string());
        }
        if config.position_tolerance < 0 {
            return Err("-- position_tolerance 不能小于 0".to_string());
        }
        if config.enforce_interval == Duration::from_millis(0) {
            return Err("-- enforce_interval_ms 不能为 0".to_string());
        }
        if config.wait_timeout == Duration::from_millis(0) {
            return Err("-- wait_timeout_ms 不能为 0".to_string());
        }

        Ok((config, config_path))
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

        if !matched && !context.class_hints.is_empty() {
            let mut class_buf = vec![0u16; 256];
            let copied = GetClassNameW(hwnd, &mut class_buf);
            if copied > 0 {
                let class_name = String::from_utf16_lossy(&class_buf[..copied as usize]).to_lowercase();
                matched = context
                    .class_hints
                    .iter()
                    .any(|hint| class_name == *hint || class_name.contains(hint));
            }
        }

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

    fn find_windows(pids: &[u32], class_hints: &[String], title_hints: &[String]) -> Vec<HWND> {
        if pids.is_empty() && class_hints.is_empty() && title_hints.is_empty() {
            return Vec::new();
        }

        let mut context = SearchContext {
            pids: pids.iter().copied().collect(),
            class_hints: class_hints.to_vec(),
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

    fn target_rect(config: &Config) -> RECT {
        unsafe {
            let screen_width = GetSystemMetrics(SM_CXSCREEN);
            let screen_height = GetSystemMetrics(SM_CYSCREEN);
            let left = screen_width - config.target_width;
            let top = screen_height - config.target_height - config.bottom_margin;
            RECT {
                left,
                top,
                right: left + config.target_width,
                bottom: top + config.target_height,
            }
        }
    }

    fn near_equal(a: i32, b: i32, tolerance: i32) -> bool {
        (a - b).abs() <= tolerance
    }

    fn is_window_in_target(hwnd: HWND, config: &Config) -> bool {
        unsafe {
            let mut current = RECT::default();
            if GetWindowRect(hwnd, &mut current).is_err() {
                return false;
            }
            let target = target_rect(config);
            near_equal(current.left, target.left, config.position_tolerance)
                && near_equal(current.top, target.top, config.position_tolerance)
                && near_equal(current.right, target.right, config.position_tolerance)
                && near_equal(current.bottom, target.bottom, config.position_tolerance)
        }
    }

    fn apply_window_layout(hwnd: HWND, config: &Config) -> bool {
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return false;
            }

            let _ = ShowWindow(hwnd, SW_RESTORE);
            let target = target_rect(config);
            let width = target.right - target.left;
            let height = target.bottom - target.top;

            SetWindowPos(
                hwnd,
                None,
                target.left,
                target.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .is_ok()
        }
    }

    fn ensure_window_layout(hwnd: HWND, config: &Config) -> bool {
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return false;
            }
        }

        if !is_window_in_target(hwnd, config) {
            if !apply_window_layout(hwnd, config) {
                return false;
            }

            sleep(Duration::from_millis(50));
            return is_window_in_target(hwnd, config);
        }

        true
    }

    pub fn run() {
        let (config, config_path) = match load_config() {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("{}", err);
                std::process::exit(2);
            }
        };
        let wow_exe_path = resolve_wow_exe_path(&config.wow_exe, &config_path);

        if !wow_exe_path.exists() {
            eprintln!(
                "未找到 {} (来自配置文件 {})",
                wow_exe_path.display(),
                config_path.display()
            );
            std::process::exit(1);
        }

        let before_launch: HashSet<u32> = collect_processes().into_iter().map(|p| p.pid).collect();

        let mut child = match Command::new(&wow_exe_path).args(&config.wow_args).spawn() {
            Ok(child) => child,
            Err(err) => {
                eprintln!("启动 {} 失败: {}", wow_exe_path.display(), err);
                std::process::exit(1);
            }
        };

        let launched_pid = child.id();
        let start = Instant::now();
        let mut launcher_exit_pid: Option<u32> = None;

        println!(
            "已启动 WoW，等待窗口并固定到右下角小窗... (类名关键字: {:?}, 标题关键字: {:?})",
            config.class_hints, config.title_hints
        );

        let mut stable_hits: u32 = 0;
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

            if start.elapsed() >= config.wait_after_launch {
                let pids = collect_candidate_pids(launched_pid, launcher_exit_pid, &before_launch);
                let windows = find_windows(&pids, &config.class_hints, &config.title_hints);

                let mut any_stable = false;
                for hwnd in windows {
                    if ensure_window_layout(hwnd, &config) {
                        any_stable = true;
                    }
                }

                if any_stable {
                    stable_hits += 1;
                } else {
                    stable_hits = 0;
                }

                if stable_hits >= config.stable_confirmations {
                    break true;
                }
            }

            if start.elapsed() > config.wait_timeout {
                break false;
            }

            sleep(config.enforce_interval);
        };

        if !first_layout_ok {
            eprintln!("超时：未能找到并固定 WoW 主窗口，请确认是窗口模式启动。");
            std::process::exit(1);
        }

        println!(
            "已将 WoW 固定为右下角小窗（{}x{}），启动器自动退出",
            config.target_width, config.target_height
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
