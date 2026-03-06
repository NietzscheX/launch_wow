#[cfg(target_os = "windows")]
mod win_launcher {
    use serde::Deserialize;
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

    const CONFIG_FILE_NAMES: &[&str] = &["launch_apps.toml", "launch_wow.toml"];
    const CONFIG_ENV_VARS: &[&str] = &["LAUNCH_APPS_CONFIG", "LAUNCH_WOW_CONFIG"];

    const LEGACY_DEFAULT_EXE: &str = "Wow.exe";
    const LEGACY_DEFAULT_ARGS: &[&str] = &["-windowed"];
    const LEGACY_DEFAULT_PROCESS_NAMES: &[&str] = &[
        "wow.exe",
        "wow-64.exe",
        "wowclassic.exe",
        "wowclassict.exe",
        "wowclasst.exe",
        "wowb.exe",
    ];
    const LEGACY_DEFAULT_TITLE_HINTS: &[&str] = &["World of Warcraft"];
    const LEGACY_DEFAULT_CLASS_HINTS: &[&str] = &["GxWindowClass"];

    const DEFAULT_TARGET_X: i32 = 100;
    const DEFAULT_TARGET_Y: i32 = 100;
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

    #[derive(Debug, Clone, Deserialize)]
    #[serde(default)]
    struct TomlConfig {
        defaults: TomlDefaults,
        apps: Vec<TomlAppConfig>,

        // 旧版单程序配置兼容字段。
        wow_exe: Option<String>,
        wow_args: Option<Vec<String>>,
        process_names: Option<Vec<String>>,
        class_hints: Option<Vec<String>>,
        title_hints: Option<Vec<String>>,
        target_width: Option<i32>,
        target_height: Option<i32>,
        bottom_margin: Option<i32>,
        wait_timeout_ms: Option<u64>,
        wait_after_launch_ms: Option<u64>,
        enforce_interval_ms: Option<u64>,
        stable_confirmations: Option<u32>,
        position_tolerance: Option<i32>,
    }

    impl Default for TomlConfig {
        fn default() -> Self {
            Self {
                defaults: TomlDefaults::default(),
                apps: Vec::new(),
                wow_exe: None,
                wow_args: None,
                process_names: None,
                class_hints: None,
                title_hints: None,
                target_width: None,
                target_height: None,
                bottom_margin: None,
                wait_timeout_ms: None,
                wait_after_launch_ms: None,
                enforce_interval_ms: None,
                stable_confirmations: None,
                position_tolerance: None,
            }
        }
    }

    impl TomlConfig {
        fn has_legacy_fields(&self) -> bool {
            self.wow_exe.is_some()
                || self.wow_args.is_some()
                || self.process_names.is_some()
                || self.class_hints.is_some()
                || self.title_hints.is_some()
                || self.target_width.is_some()
                || self.target_height.is_some()
                || self.bottom_margin.is_some()
                || self.wait_timeout_ms.is_some()
                || self.wait_after_launch_ms.is_some()
                || self.enforce_interval_ms.is_some()
                || self.stable_confirmations.is_some()
                || self.position_tolerance.is_some()
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(default)]
    struct TomlDefaults {
        wait_timeout_ms: u64,
        wait_after_launch_ms: u64,
        enforce_interval_ms: u64,
        stable_confirmations: u32,
        position_tolerance: i32,
    }

    impl Default for TomlDefaults {
        fn default() -> Self {
            Self {
                wait_timeout_ms: DEFAULT_WAIT_TIMEOUT_MS,
                wait_after_launch_ms: DEFAULT_WAIT_AFTER_LAUNCH_MS,
                enforce_interval_ms: DEFAULT_ENFORCE_INTERVAL_MS,
                stable_confirmations: DEFAULT_STABLE_CONFIRMATIONS,
                position_tolerance: DEFAULT_POSITION_TOLERANCE,
            }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    #[serde(default)]
    struct TomlAppConfig {
        enabled: bool,
        name: String,
        exe: String,
        args: Vec<String>,
        process_names: Vec<String>,
        class_hints: Vec<String>,
        title_hints: Vec<String>,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        wait_timeout_ms: Option<u64>,
        wait_after_launch_ms: Option<u64>,
        enforce_interval_ms: Option<u64>,
        stable_confirmations: Option<u32>,
        position_tolerance: Option<i32>,
    }

    impl Default for TomlAppConfig {
        fn default() -> Self {
            Self {
                enabled: true,
                name: String::new(),
                exe: String::new(),
                args: Vec::new(),
                process_names: Vec::new(),
                class_hints: Vec::new(),
                title_hints: Vec::new(),
                x: DEFAULT_TARGET_X,
                y: DEFAULT_TARGET_Y,
                width: DEFAULT_TARGET_WIDTH,
                height: DEFAULT_TARGET_HEIGHT,
                wait_timeout_ms: None,
                wait_after_launch_ms: None,
                enforce_interval_ms: None,
                stable_confirmations: None,
                position_tolerance: None,
            }
        }
    }

    #[derive(Clone)]
    struct LaunchConfig {
        apps: Vec<AppConfig>,
    }

    #[derive(Clone)]
    struct AppConfig {
        name: String,
        exe: String,
        args: Vec<String>,
        process_names: Vec<String>,
        class_hints: Vec<String>,
        title_hints: Vec<String>,
        placement: WindowPlacement,
        wait_timeout: Duration,
        wait_after_launch: Duration,
        enforce_interval: Duration,
        stable_confirmations: u32,
        position_tolerance: i32,
    }

    #[derive(Clone)]
    enum WindowPlacement {
        Absolute {
            x: i32,
            y: i32,
            width: i32,
            height: i32,
        },
        BottomRight {
            width: i32,
            height: i32,
            bottom_margin: i32,
        },
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
                        exe_name: wide_to_string(&entry.szExeFile).to_lowercase(),
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

    fn trim_args(raw: Vec<String>) -> Vec<String> {
        raw.into_iter()
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect()
    }

    fn normalize_matchers(raw: Vec<String>) -> Vec<String> {
        let mut normalized: Vec<String> = raw
            .into_iter()
            .map(|part| part.trim().to_lowercase())
            .filter(|part| !part.is_empty())
            .collect();
        normalized.sort_unstable();
        normalized.dedup();
        normalized
    }

    fn normalize_matchers_with_defaults(raw: Vec<String>, defaults: &[&str]) -> Vec<String> {
        let normalized = normalize_matchers(raw);
        if normalized.is_empty() {
            defaults.iter().map(|part| part.to_lowercase()).collect()
        } else {
            normalized
        }
    }

    fn canonical_process_name(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let file_name = Path::new(trimmed)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(trimmed);
        let lower = file_name.trim().to_lowercase();
        if lower.is_empty() {
            return None;
        }

        if lower.ends_with(".exe") {
            Some(lower)
        } else {
            Some(format!("{}.exe", lower))
        }
    }

    fn default_process_names_for_exe(exe: &str) -> Vec<String> {
        canonical_process_name(exe).into_iter().collect()
    }

    fn normalize_process_names(raw: Vec<String>, exe: &str, defaults: &[&str]) -> Vec<String> {
        let mut names: Vec<String> = raw
            .into_iter()
            .filter_map(|part| canonical_process_name(&part))
            .collect();

        if names.is_empty() {
            names = defaults
                .iter()
                .filter_map(|part| canonical_process_name(part))
                .collect();
        }

        if names.is_empty() {
            names = default_process_names_for_exe(exe);
        }

        names.sort_unstable();
        names.dedup();
        names
    }

    fn infer_app_name(preferred: &str, exe: &str) -> String {
        let trimmed = preferred.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }

        Path::new(exe)
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("app")
            .to_string()
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

        for env_name in CONFIG_ENV_VARS {
            if let Ok(raw) = std::env::var(env_name) {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    candidates.push(PathBuf::from(trimmed));
                }
            }
        }

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                for file_name in CONFIG_FILE_NAMES {
                    candidates.push(dir.join(file_name));
                }
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            for file_name in CONFIG_FILE_NAMES {
                candidates.push(cwd.join(file_name));
            }
        }

        for file_name in CONFIG_FILE_NAMES {
            candidates.push(PathBuf::from(file_name));
        }

        dedup_paths(candidates)
    }

    fn default_config_targets() -> Vec<PathBuf> {
        let mut targets = Vec::new();

        for env_name in CONFIG_ENV_VARS {
            if let Ok(raw) = std::env::var(env_name) {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    targets.push(PathBuf::from(trimmed));
                }
            }
        }

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                targets.push(dir.join(CONFIG_FILE_NAMES[0]));
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            targets.push(cwd.join(CONFIG_FILE_NAMES[0]));
        }

        targets.push(PathBuf::from(CONFIG_FILE_NAMES[0]));
        dedup_paths(targets)
    }

    fn find_existing_config() -> Option<PathBuf> {
        config_candidates().into_iter().find(|path| path.exists())
    }

    fn create_default_config() -> Result<PathBuf, String> {
        let default_content = default_config_template();
        let targets = default_config_targets();
        let mut errors = Vec::new();

        for target in targets {
            if let Some(parent) = target.parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(err) = fs::create_dir_all(parent) {
                        errors.push(format!("创建目录失败 {}: {}", parent.display(), err));
                        continue;
                    }
                }
            }

            match fs::write(&target, &default_content) {
                Ok(_) => return Ok(target),
                Err(err) => errors.push(format!("写入失败 {}: {}", target.display(), err)),
            }
        }

        Err(format!(
            "无法自动创建配置文件 {}。尝试结果:\n{}",
            CONFIG_FILE_NAMES[0],
            errors.join("\n")
        ))
    }

    fn resolve_exe_path(exe: &str, config_path: &Path) -> PathBuf {
        let candidate = Path::new(exe);
        if candidate.is_absolute() {
            return candidate.to_path_buf();
        }

        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(candidate)
    }

    fn default_config_template() -> String {
        let process_names = LEGACY_DEFAULT_PROCESS_NAMES
            .iter()
            .map(|name| format!("\"{}\"", name))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "# launch_apps.toml\n\
             # 相对路径会按“配置文件所在目录”解析。\n\
             # 多个 [[apps]] 会按顺序依次启动，并把窗口固定到指定位置和大小。\n\
             \n\
             [defaults]\n\
             wait_timeout_ms = {}\n\
             wait_after_launch_ms = {}\n\
             enforce_interval_ms = {}\n\
             stable_confirmations = {}\n\
             position_tolerance = {}\n\
             \n\
             [[apps]]\n\
             name = \"wow\"\n\
             exe = \"{}\"\n\
             args = [\"{}\"]\n\
             process_names = [{}]\n\
             class_hints = [\"{}\"]\n\
             title_hints = [\"{}\"]\n\
             x = {}\n\
             y = {}\n\
             width = {}\n\
             height = {}\n\
             \n\
             [[apps]]\n\
             enabled = false\n\
             name = \"notepad\"\n\
             exe = \"C:\\\\Windows\\\\System32\\\\notepad.exe\"\n\
             title_hints = [\"notepad\", \"记事本\"]\n\
             x = 650\n\
             y = 100\n\
             width = 900\n\
             height = 700\n",
            DEFAULT_WAIT_TIMEOUT_MS,
            DEFAULT_WAIT_AFTER_LAUNCH_MS,
            DEFAULT_ENFORCE_INTERVAL_MS,
            DEFAULT_STABLE_CONFIRMATIONS,
            DEFAULT_POSITION_TOLERANCE,
            LEGACY_DEFAULT_EXE,
            LEGACY_DEFAULT_ARGS[0],
            process_names,
            LEGACY_DEFAULT_CLASS_HINTS[0],
            LEGACY_DEFAULT_TITLE_HINTS[0],
            DEFAULT_TARGET_X,
            DEFAULT_TARGET_Y,
            DEFAULT_TARGET_WIDTH,
            DEFAULT_TARGET_HEIGHT
        )
    }

    fn build_legacy_app(file_config: &TomlConfig) -> AppConfig {
        let exe = file_config
            .wow_exe
            .as_deref()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .unwrap_or(LEGACY_DEFAULT_EXE)
            .to_string();

        let args = file_config
            .wow_args
            .clone()
            .map(trim_args)
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(|| {
                LEGACY_DEFAULT_ARGS
                    .iter()
                    .map(|part| (*part).to_string())
                    .collect()
            });

        AppConfig {
            name: infer_app_name("", &exe),
            exe: exe.clone(),
            args,
            process_names: normalize_process_names(
                file_config.process_names.clone().unwrap_or_default(),
                &exe,
                LEGACY_DEFAULT_PROCESS_NAMES,
            ),
            class_hints: normalize_matchers_with_defaults(
                file_config.class_hints.clone().unwrap_or_default(),
                LEGACY_DEFAULT_CLASS_HINTS,
            ),
            title_hints: normalize_matchers_with_defaults(
                file_config.title_hints.clone().unwrap_or_default(),
                LEGACY_DEFAULT_TITLE_HINTS,
            ),
            placement: WindowPlacement::BottomRight {
                width: file_config.target_width.unwrap_or(DEFAULT_TARGET_WIDTH),
                height: file_config.target_height.unwrap_or(DEFAULT_TARGET_HEIGHT),
                bottom_margin: file_config.bottom_margin.unwrap_or(DEFAULT_BOTTOM_MARGIN),
            },
            wait_timeout: Duration::from_millis(
                file_config
                    .wait_timeout_ms
                    .unwrap_or(file_config.defaults.wait_timeout_ms),
            ),
            wait_after_launch: Duration::from_millis(
                file_config
                    .wait_after_launch_ms
                    .unwrap_or(file_config.defaults.wait_after_launch_ms),
            ),
            enforce_interval: Duration::from_millis(
                file_config
                    .enforce_interval_ms
                    .unwrap_or(file_config.defaults.enforce_interval_ms),
            ),
            stable_confirmations: file_config
                .stable_confirmations
                .unwrap_or(file_config.defaults.stable_confirmations),
            position_tolerance: file_config
                .position_tolerance
                .unwrap_or(file_config.defaults.position_tolerance),
        }
    }

    fn build_app_config(entry: TomlAppConfig, defaults: &TomlDefaults) -> AppConfig {
        let exe = entry.exe.trim().to_string();

        AppConfig {
            name: infer_app_name(&entry.name, &exe),
            exe: exe.clone(),
            args: trim_args(entry.args),
            process_names: normalize_process_names(entry.process_names, &exe, &[]),
            class_hints: normalize_matchers(entry.class_hints),
            title_hints: normalize_matchers(entry.title_hints),
            placement: WindowPlacement::Absolute {
                x: entry.x,
                y: entry.y,
                width: entry.width,
                height: entry.height,
            },
            wait_timeout: Duration::from_millis(
                entry.wait_timeout_ms.unwrap_or(defaults.wait_timeout_ms),
            ),
            wait_after_launch: Duration::from_millis(
                entry
                    .wait_after_launch_ms
                    .unwrap_or(defaults.wait_after_launch_ms),
            ),
            enforce_interval: Duration::from_millis(
                entry
                    .enforce_interval_ms
                    .unwrap_or(defaults.enforce_interval_ms),
            ),
            stable_confirmations: entry
                .stable_confirmations
                .unwrap_or(defaults.stable_confirmations),
            position_tolerance: entry
                .position_tolerance
                .unwrap_or(defaults.position_tolerance),
        }
    }

    fn validate_app_config(app: &AppConfig) -> Result<(), String> {
        if app.exe.trim().is_empty() {
            return Err(format!("应用 [{}] 的 exe 不能为空", app.name));
        }

        let (width, height) = placement_size(&app.placement);
        if width <= 0 {
            return Err(format!("应用 [{}] 的 width 必须大于 0", app.name));
        }
        if height <= 0 {
            return Err(format!("应用 [{}] 的 height 必须大于 0", app.name));
        }
        if app.stable_confirmations == 0 {
            return Err(format!(
                "应用 [{}] 的 stable_confirmations 必须大于 0",
                app.name
            ));
        }
        if app.position_tolerance < 0 {
            return Err(format!(
                "应用 [{}] 的 position_tolerance 不能小于 0",
                app.name
            ));
        }
        if app.enforce_interval == Duration::from_millis(0) {
            return Err(format!(
                "应用 [{}] 的 enforce_interval_ms 不能为 0",
                app.name
            ));
        }
        if app.wait_timeout == Duration::from_millis(0) {
            return Err(format!("应用 [{}] 的 wait_timeout_ms 不能为 0", app.name));
        }

        Ok(())
    }

    fn load_config() -> Result<(LaunchConfig, PathBuf), String> {
        let config_path = if let Some(path) = find_existing_config() {
            path
        } else {
            let created = create_default_config()?;
            return Err(format!(
                "未找到配置文件，已生成默认配置: {}。请先按需修改配置后再运行。",
                created.display()
            ));
        };

        if !config_path.exists() {
            return Err(format!("配置文件路径不存在: {}", config_path.display()));
        }

        if fs::metadata(&config_path).is_err() {
            return Err(format!("无法访问配置文件: {}", config_path.display()));
        }

        let config_text = fs::read_to_string(&config_path)
            .map_err(|err| format!("读取配置文件失败 ({}): {}", config_path.display(), err))?;
        let file_config: TomlConfig = toml::from_str(&config_text)
            .map_err(|err| format!("解析 TOML 配置失败 ({}): {}", config_path.display(), err))?;

        let mut apps = Vec::new();
        if !file_config.apps.is_empty() {
            for entry in file_config.apps.clone() {
                if !entry.enabled {
                    continue;
                }
                apps.push(build_app_config(entry, &file_config.defaults));
            }
        } else if file_config.has_legacy_fields() {
            apps.push(build_legacy_app(&file_config));
        } else {
            return Err(format!(
                "配置文件中没有可启动的应用，请添加 [[apps]] 条目 ({})",
                config_path.display()
            ));
        }

        if apps.is_empty() {
            return Err("配置文件中的所有 [[apps]] 都被禁用了，没有可启动项".to_string());
        }

        for app in &apps {
            validate_app_config(app)?;
        }

        Ok((LaunchConfig { apps }, config_path))
    }

    fn collect_candidate_pids(
        launched_pid: u32,
        before_launch: &HashSet<u32>,
        process_names: &[String],
    ) -> Vec<u32> {
        let processes = collect_processes();
        let mut candidates = HashSet::new();
        candidates.insert(launched_pid);

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

        for proc in &processes {
            if before_launch.contains(&proc.pid) {
                continue;
            }
            if process_names.iter().any(|name| proc.exe_name == *name) {
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
                let class_name =
                    String::from_utf16_lossy(&class_buf[..copied as usize]).to_lowercase();
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
                    let title =
                        String::from_utf16_lossy(&title_buf[..copied as usize]).to_lowercase();
                    matched = context.title_hints.iter().any(|hint| title.contains(hint));
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

    fn target_rect(placement: &WindowPlacement) -> RECT {
        match placement {
            WindowPlacement::Absolute {
                x,
                y,
                width,
                height,
            } => RECT {
                left: *x,
                top: *y,
                right: *x + *width,
                bottom: *y + *height,
            },
            WindowPlacement::BottomRight {
                width,
                height,
                bottom_margin,
            } => unsafe {
                let screen_width = GetSystemMetrics(SM_CXSCREEN);
                let screen_height = GetSystemMetrics(SM_CYSCREEN);
                let left = screen_width - *width;
                let top = screen_height - *height - *bottom_margin;
                RECT {
                    left,
                    top,
                    right: left + *width,
                    bottom: top + *height,
                }
            },
        }
    }

    fn placement_size(placement: &WindowPlacement) -> (i32, i32) {
        match placement {
            WindowPlacement::Absolute { width, height, .. }
            | WindowPlacement::BottomRight { width, height, .. } => (*width, *height),
        }
    }

    fn placement_description(placement: &WindowPlacement) -> String {
        match placement {
            WindowPlacement::Absolute {
                x,
                y,
                width,
                height,
            } => format!("x={}, y={}, {}x{}", x, y, width, height),
            WindowPlacement::BottomRight {
                width,
                height,
                bottom_margin,
            } => format!("右下角, 底边距={}, {}x{}", bottom_margin, width, height),
        }
    }

    fn near_equal(a: i32, b: i32, tolerance: i32) -> bool {
        (a - b).abs() <= tolerance
    }

    fn is_window_in_target(hwnd: HWND, app: &AppConfig) -> bool {
        unsafe {
            let mut current = RECT::default();
            if GetWindowRect(hwnd, &mut current).is_err() {
                return false;
            }
            let target = target_rect(&app.placement);
            near_equal(current.left, target.left, app.position_tolerance)
                && near_equal(current.top, target.top, app.position_tolerance)
                && near_equal(current.right, target.right, app.position_tolerance)
                && near_equal(current.bottom, target.bottom, app.position_tolerance)
        }
    }

    fn apply_window_layout(hwnd: HWND, app: &AppConfig) -> bool {
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return false;
            }

            let _ = ShowWindow(hwnd, SW_RESTORE);
            let target = target_rect(&app.placement);
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

    fn ensure_window_layout(hwnd: HWND, app: &AppConfig) -> bool {
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return false;
            }
        }

        if !is_window_in_target(hwnd, app) {
            if !apply_window_layout(hwnd, app) {
                return false;
            }

            sleep(Duration::from_millis(50));
            return is_window_in_target(hwnd, app);
        }

        true
    }

    fn launch_app(app: &AppConfig, config_path: &Path) -> Result<(), String> {
        let exe_path = resolve_exe_path(&app.exe, config_path);
        if !exe_path.exists() {
            return Err(format!(
                "应用 [{}] 未找到可执行文件 {} (来自配置文件 {})",
                app.name,
                exe_path.display(),
                config_path.display()
            ));
        }

        let before_launch: HashSet<u32> = collect_processes()
            .into_iter()
            .map(|proc| proc.pid)
            .collect();

        let launched_pid = Command::new(&exe_path)
            .args(&app.args)
            .spawn()
            .map_err(|err| {
                format!(
                    "应用 [{}] 启动失败 ({}): {}",
                    app.name,
                    exe_path.display(),
                    err
                )
            })?
            .id();
        let start = Instant::now();

        println!(
            "启动 [{}] -> {}，目标位置: {}",
            app.name,
            exe_path.display(),
            placement_description(&app.placement)
        );

        let mut stable_hits: u32 = 0;
        let layout_ok = loop {
            if start.elapsed() >= app.wait_after_launch {
                let pids = collect_candidate_pids(launched_pid, &before_launch, &app.process_names);
                let windows = find_windows(&pids, &app.class_hints, &app.title_hints);

                let mut any_stable = false;
                for hwnd in windows {
                    if ensure_window_layout(hwnd, app) {
                        any_stable = true;
                    }
                }

                if any_stable {
                    stable_hits += 1;
                } else {
                    stable_hits = 0;
                }

                if stable_hits >= app.stable_confirmations {
                    break true;
                }
            }

            if start.elapsed() > app.wait_timeout {
                break false;
            }

            sleep(app.enforce_interval);
        };

        if !layout_ok {
            return Err(format!(
                "应用 [{}] 超时：未能找到并固定窗口，请确认它会创建可见窗口，或补充 process_names/class_hints/title_hints",
                app.name
            ));
        }

        let (width, height) = placement_size(&app.placement);
        println!("应用 [{}] 已固定完成 ({}x{})", app.name, width, height);
        Ok(())
    }

    pub fn run() {
        let (config, config_path) = match load_config() {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("{}", err);
                std::process::exit(2);
            }
        };

        for app in &config.apps {
            if let Err(err) = launch_app(app, &config_path) {
                eprintln!("{}", err);
                std::process::exit(1);
            }
        }

        println!("全部应用处理完成，启动器退出");
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
