#[cfg(target_os = "windows")]
mod win_launcher {
    use serde::Deserialize;
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::fs;
    use std::mem::size_of;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT};
    use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
        PROCESS_VM_WRITE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_RETURN, VK_TAB,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetForegroundWindow,
        SetWindowPos, ShowWindow, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER,
        SWP_SHOWWINDOW, SW_RESTORE,
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
    const DEFAULT_WOW_STATE_POLL_INTERVAL_MS: u64 = 500;
    const DEFAULT_WOW_LOGIN_TIMEOUT_MS: u64 = 60_000;
    const DEFAULT_WOW_KEY_INPUT_DELAY_MS: u64 = 40;
    const DEFAULT_WOW_AFTER_LOGIN_SUBMIT_DELAY_MS: u64 = 1_500;
    const DEFAULT_WOW_AFTER_ENTER_WORLD_DELAY_MS: u64 = 6_000;
    const DEFAULT_WOW_LOGIN_FALLBACK_AFTER_MS: u64 = 8_000;

    const WOW_GAME_STATE_ADDRESS: usize = 0x90753C;
    const WOW_SELECTED_CHARACTER_INDEX_ADDRESS: usize = 0xAD7414;
    const WOW_STATE_ACCOUNT_SELECT: u32 = 0x01;
    const WOW_STATE_CHARACTER_SELECT_OR_INGAME: u32 = 0x15;
    const WOW_STATE_LOGIN_SCREEN: u32 = 0x16;

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
        wow_auto_login: Option<bool>,
        wow_account: Option<String>,
        wow_password: Option<String>,
        wow_password_env: Option<String>,
        wow_character_index: Option<u32>,
        wow_state_poll_interval_ms: Option<u64>,
        wow_login_timeout_ms: Option<u64>,
        wow_key_input_delay_ms: Option<u64>,
        wow_after_login_submit_delay_ms: Option<u64>,
        wow_after_enter_world_delay_ms: Option<u64>,
        wow_login_fallback_after_ms: Option<u64>,
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
                wow_auto_login: None,
                wow_account: None,
                wow_password: None,
                wow_password_env: None,
                wow_character_index: None,
                wow_state_poll_interval_ms: None,
                wow_login_timeout_ms: None,
                wow_key_input_delay_ms: None,
                wow_after_login_submit_delay_ms: None,
                wow_after_enter_world_delay_ms: None,
                wow_login_fallback_after_ms: None,
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
                || self.wow_auto_login.is_some()
                || self.wow_account.is_some()
                || self.wow_password.is_some()
                || self.wow_password_env.is_some()
                || self.wow_character_index.is_some()
                || self.wow_state_poll_interval_ms.is_some()
                || self.wow_login_timeout_ms.is_some()
                || self.wow_key_input_delay_ms.is_some()
                || self.wow_after_login_submit_delay_ms.is_some()
                || self.wow_after_enter_world_delay_ms.is_some()
                || self.wow_login_fallback_after_ms.is_some()
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
        wow_auto_login: bool,
        wow_account: String,
        wow_password: String,
        wow_password_env: String,
        wow_character_index: u32,
        wow_state_poll_interval_ms: Option<u64>,
        wow_login_timeout_ms: Option<u64>,
        wow_key_input_delay_ms: Option<u64>,
        wow_after_login_submit_delay_ms: Option<u64>,
        wow_after_enter_world_delay_ms: Option<u64>,
        wow_login_fallback_after_ms: Option<u64>,
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
                wow_auto_login: false,
                wow_account: String::new(),
                wow_password: String::new(),
                wow_password_env: String::new(),
                wow_character_index: 0,
                wow_state_poll_interval_ms: None,
                wow_login_timeout_ms: None,
                wow_key_input_delay_ms: None,
                wow_after_login_submit_delay_ms: None,
                wow_after_enter_world_delay_ms: None,
                wow_login_fallback_after_ms: None,
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
        wow_login: Option<WowLoginConfig>,
    }

    #[derive(Clone)]
    struct WowLoginConfig {
        account: String,
        password: String,
        character_index: u32,
        state_poll_interval: Duration,
        login_timeout: Duration,
        key_input_delay: Duration,
        after_login_submit_delay: Duration,
        after_enter_world_delay: Duration,
        login_fallback_after: Duration,
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

    fn resolve_wow_password(
        app_name: &str,
        password: &str,
        password_env: &str,
    ) -> Result<String, String> {
        let trimmed_password = password.trim();
        if !trimmed_password.is_empty() {
            return Ok(trimmed_password.to_string());
        }

        let trimmed_env = password_env.trim();
        if trimmed_env.is_empty() {
            return Err(format!(
                "应用 [{}] 启用了 WoW 自动登录，但 wow_password 为空",
                app_name
            ));
        }

        let value = std::env::var(trimmed_env).map_err(|_| {
            format!(
                "应用 [{}] 启用了 WoW 自动登录，但环境变量 {} 不存在",
                app_name, trimmed_env
            )
        })?;

        let trimmed_value = value.trim();
        if trimmed_value.is_empty() {
            return Err(format!(
                "应用 [{}] 启用了 WoW 自动登录，但环境变量 {} 为空",
                app_name, trimmed_env
            ));
        }

        Ok(trimmed_value.to_string())
    }

    fn should_enable_wow_login(
        explicit_enabled: bool,
        account: &str,
        password: &str,
        password_env: &str,
    ) -> bool {
        explicit_enabled
            || (!account.trim().is_empty()
                && (!password.trim().is_empty() || !password_env.trim().is_empty()))
    }

    fn build_wow_login_config(
        app_name: &str,
        enabled: bool,
        account: &str,
        password: &str,
        password_env: &str,
        character_index: u32,
        state_poll_interval_ms: Option<u64>,
        login_timeout_ms: Option<u64>,
        key_input_delay_ms: Option<u64>,
        after_login_submit_delay_ms: Option<u64>,
        after_enter_world_delay_ms: Option<u64>,
        login_fallback_after_ms: Option<u64>,
    ) -> Result<Option<WowLoginConfig>, String> {
        let effective_enabled = should_enable_wow_login(enabled, account, password, password_env);
        if !effective_enabled {
            return Ok(None);
        }

        let trimmed_account = account.trim();
        if trimmed_account.is_empty() {
            return Err(format!(
                "应用 [{}] 启用了 WoW 自动登录，但 wow_account 为空",
                app_name
            ));
        }

        let resolved_password = resolve_wow_password(app_name, password, password_env)?;
        let state_poll_interval = Duration::from_millis(
            state_poll_interval_ms.unwrap_or(DEFAULT_WOW_STATE_POLL_INTERVAL_MS),
        );
        let login_timeout =
            Duration::from_millis(login_timeout_ms.unwrap_or(DEFAULT_WOW_LOGIN_TIMEOUT_MS));
        let key_input_delay =
            Duration::from_millis(key_input_delay_ms.unwrap_or(DEFAULT_WOW_KEY_INPUT_DELAY_MS));
        let after_login_submit_delay = Duration::from_millis(
            after_login_submit_delay_ms.unwrap_or(DEFAULT_WOW_AFTER_LOGIN_SUBMIT_DELAY_MS),
        );
        let after_enter_world_delay = Duration::from_millis(
            after_enter_world_delay_ms.unwrap_or(DEFAULT_WOW_AFTER_ENTER_WORLD_DELAY_MS),
        );
        let login_fallback_after = Duration::from_millis(
            login_fallback_after_ms.unwrap_or(DEFAULT_WOW_LOGIN_FALLBACK_AFTER_MS),
        );

        if state_poll_interval == Duration::from_millis(0) {
            return Err(format!(
                "应用 [{}] 的 wow_state_poll_interval_ms 不能为 0",
                app_name
            ));
        }
        if login_timeout == Duration::from_millis(0) {
            return Err(format!(
                "应用 [{}] 的 wow_login_timeout_ms 不能为 0",
                app_name
            ));
        }
        if login_fallback_after == Duration::from_millis(0) {
            return Err(format!(
                "应用 [{}] 的 wow_login_fallback_after_ms 不能为 0",
                app_name
            ));
        }

        Ok(Some(WowLoginConfig {
            account: trimmed_account.to_string(),
            password: resolved_password,
            character_index,
            state_poll_interval,
            login_timeout,
            key_input_delay,
            after_login_submit_delay,
            after_enter_world_delay,
            login_fallback_after,
        }))
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
             # WoW 4.3.4 自动登录支持账号/密码和角色索引（从 0 开始）。\n\
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
             wow_auto_login = false\n\
             wow_account = \"your-account\"\n\
             wow_password = \"your-password\"\n\
             wow_character_index = 0\n\
             wow_login_fallback_after_ms = {}\n\
             # wow_password_env = \"WOW_PASSWORD\"\n\
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
            DEFAULT_TARGET_HEIGHT,
            DEFAULT_WOW_LOGIN_FALLBACK_AFTER_MS
        )
    }

    fn build_legacy_app(file_config: &TomlConfig) -> Result<AppConfig, String> {
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

        let app_name = infer_app_name("", &exe);
        let wow_login = build_wow_login_config(
            &app_name,
            file_config.wow_auto_login.unwrap_or(false),
            file_config.wow_account.as_deref().unwrap_or(""),
            file_config.wow_password.as_deref().unwrap_or(""),
            file_config.wow_password_env.as_deref().unwrap_or(""),
            file_config.wow_character_index.unwrap_or(0),
            file_config.wow_state_poll_interval_ms,
            file_config.wow_login_timeout_ms,
            file_config.wow_key_input_delay_ms,
            file_config.wow_after_login_submit_delay_ms,
            file_config.wow_after_enter_world_delay_ms,
            file_config.wow_login_fallback_after_ms,
        )?;

        Ok(AppConfig {
            name: app_name,
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
            wow_login,
        })
    }

    fn build_app_config(
        entry: TomlAppConfig,
        defaults: &TomlDefaults,
    ) -> Result<AppConfig, String> {
        let exe = entry.exe.trim().to_string();
        let app_name = infer_app_name(&entry.name, &exe);
        let wow_login = build_wow_login_config(
            &app_name,
            entry.wow_auto_login,
            &entry.wow_account,
            &entry.wow_password,
            &entry.wow_password_env,
            entry.wow_character_index,
            entry.wow_state_poll_interval_ms,
            entry.wow_login_timeout_ms,
            entry.wow_key_input_delay_ms,
            entry.wow_after_login_submit_delay_ms,
            entry.wow_after_enter_world_delay_ms,
            entry.wow_login_fallback_after_ms,
        )?;

        Ok(AppConfig {
            name: app_name,
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
            wow_login,
        })
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
                apps.push(build_app_config(entry, &file_config.defaults)?);
            }
        } else if file_config.has_legacy_fields() {
            apps.push(build_legacy_app(&file_config)?);
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

    fn wow_state_name(state: u32) -> &'static str {
        match state {
            WOW_STATE_ACCOUNT_SELECT => "账号选择界面",
            WOW_STATE_LOGIN_SCREEN => "登录界面",
            WOW_STATE_CHARACTER_SELECT_OR_INGAME => "角色选择/游戏内",
            _ => "未知状态",
        }
    }

    fn focus_window(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        sleep(Duration::from_millis(150));
    }

    fn keyboard_input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send_input_batch(inputs: &[INPUT]) -> Result<(), String> {
        if inputs.is_empty() {
            return Ok(());
        }

        let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(format!(
                "发送键盘输入失败，期望 {} 个事件，实际 {} 个",
                inputs.len(),
                sent
            ));
        }

        Ok(())
    }

    fn send_virtual_key(vk: VIRTUAL_KEY) -> Result<(), String> {
        let inputs = [
            keyboard_input(vk, 0, KEYBD_EVENT_FLAGS(0)),
            keyboard_input(vk, 0, KEYEVENTF_KEYUP),
        ];
        send_input_batch(&inputs)
    }

    fn send_modified_key(modifier: VIRTUAL_KEY, key: VIRTUAL_KEY) -> Result<(), String> {
        let inputs = [
            keyboard_input(modifier, 0, KEYBD_EVENT_FLAGS(0)),
            keyboard_input(key, 0, KEYBD_EVENT_FLAGS(0)),
            keyboard_input(key, 0, KEYEVENTF_KEYUP),
            keyboard_input(modifier, 0, KEYEVENTF_KEYUP),
        ];
        send_input_batch(&inputs)
    }

    fn send_unicode_text(text: &str, key_delay: Duration) -> Result<(), String> {
        for ch in text.encode_utf16() {
            let inputs = [
                keyboard_input(VIRTUAL_KEY(0), ch, KEYEVENTF_UNICODE),
                keyboard_input(VIRTUAL_KEY(0), ch, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ];
            send_input_batch(&inputs)?;
            if key_delay > Duration::from_millis(0) {
                sleep(key_delay);
            }
        }
        Ok(())
    }

    fn clear_active_field(key_delay: Duration) -> Result<(), String> {
        send_modified_key(VK_CONTROL, VIRTUAL_KEY(0x41))?;
        if key_delay > Duration::from_millis(0) {
            sleep(key_delay);
        }
        send_virtual_key(VK_BACK)?;
        if key_delay > Duration::from_millis(0) {
            sleep(key_delay);
        }
        Ok(())
    }

    fn submit_wow_credentials(
        hwnd: HWND,
        wow_login: &WowLoginConfig,
        app_name: &str,
    ) -> Result<(), String> {
        focus_window(hwnd);
        clear_active_field(wow_login.key_input_delay)?;
        send_unicode_text(&wow_login.account, wow_login.key_input_delay)?;
        send_virtual_key(VK_TAB)?;
        if wow_login.key_input_delay > Duration::from_millis(0) {
            sleep(wow_login.key_input_delay);
        }
        clear_active_field(wow_login.key_input_delay)?;
        send_unicode_text(&wow_login.password, wow_login.key_input_delay)?;
        send_virtual_key(VK_RETURN)?;
        println!("应用 [{}] 已提交 WoW 账号密码", app_name);
        Ok(())
    }

    fn open_process_for_wow(pid: u32) -> Result<HANDLE, String> {
        unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION
                    | PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_VM_OPERATION,
                false,
                pid,
            )
            .map_err(|err| format!("打开 WoW 进程失败 (pid={}): {}", pid, err))
        }
    }

    fn read_process_u32(handle: HANDLE, address: usize) -> Result<u32, String> {
        let mut value = 0u32;
        let mut bytes_read = 0usize;
        unsafe {
            ReadProcessMemory(
                handle,
                address as *const c_void,
                &mut value as *mut u32 as *mut c_void,
                size_of::<u32>(),
                Some(&mut bytes_read),
            )
            .map_err(|err| format!("读取 WoW 内存失败 (0x{:X}): {}", address, err))?;
        }

        if bytes_read != size_of::<u32>() {
            return Err(format!(
                "读取 WoW 内存长度异常 (0x{:X}): {}",
                address, bytes_read
            ));
        }

        Ok(value)
    }

    fn write_process_u32(handle: HANDLE, address: usize, value: u32) -> Result<(), String> {
        let mut bytes_written = 0usize;
        unsafe {
            WriteProcessMemory(
                handle,
                address as *const c_void,
                &value as *const u32 as *const c_void,
                size_of::<u32>(),
                Some(&mut bytes_written),
            )
            .map_err(|err| format!("写入 WoW 内存失败 (0x{:X}): {}", address, err))?;
        }

        if bytes_written != size_of::<u32>() {
            return Err(format!(
                "写入 WoW 内存长度异常 (0x{:X}): {}",
                address, bytes_written
            ));
        }

        Ok(())
    }

    fn select_target_pid(candidate_pids: &[u32], process_names: &[String]) -> Option<u32> {
        if candidate_pids.is_empty() {
            return None;
        }

        let processes = collect_processes();
        for pid in candidate_pids {
            if processes.iter().any(|proc| {
                proc.pid == *pid && process_names.iter().any(|name| proc.exe_name == *name)
            }) {
                return Some(*pid);
            }
        }

        candidate_pids.first().copied()
    }

    fn run_wow_auto_login(
        app: &AppConfig,
        launched_pid: u32,
        before_launch: &HashSet<u32>,
    ) -> Result<(), String> {
        let wow_login = match &app.wow_login {
            Some(config) => config,
            None => return Ok(()),
        };

        println!(
            "应用 [{}] 开始执行 WoW 自动登录，目标角色索引: {}",
            app.name, wow_login.character_index
        );

        let start = Instant::now();
        let mut credentials_submitted = false;
        let mut account_select_confirmed = false;
        let mut enter_world_started_at: Option<Instant> = None;
        let mut last_state: Option<u32> = None;
        let mut first_window_seen_at: Option<Instant> = None;
        let mut fallback_login_attempted = false;

        loop {
            if start.elapsed() > wow_login.login_timeout {
                let state_text = last_state
                    .map(|state| format!("0x{:X} ({})", state, wow_state_name(state)))
                    .unwrap_or_else(|| "未知".to_string());
                return Err(format!(
                    "应用 [{}] WoW 自动登录超时，最后状态: {}",
                    app.name, state_text
                ));
            }

            let candidate_pids =
                collect_candidate_pids(launched_pid, before_launch, &app.process_names);
            let Some(target_pid) = select_target_pid(&candidate_pids, &app.process_names) else {
                sleep(wow_login.state_poll_interval);
                continue;
            };
            let windows = find_windows(&candidate_pids, &app.class_hints, &app.title_hints);
            let Some(hwnd) = windows.first().copied() else {
                sleep(wow_login.state_poll_interval);
                continue;
            };
            if first_window_seen_at.is_none() {
                first_window_seen_at = Some(Instant::now());
            }

            let handle = open_process_for_wow(target_pid)?;
            let state_result = read_process_u32(handle, WOW_GAME_STATE_ADDRESS);
            close_handle(handle);
            let state = state_result?;
            if last_state != Some(state) {
                println!(
                    "应用 [{}] WoW 状态变化 -> 0x{:X} ({})",
                    app.name,
                    state,
                    wow_state_name(state)
                );
                last_state = Some(state);
            }

            if !credentials_submitted
                && enter_world_started_at.is_none()
                && !fallback_login_attempted
                && state != WOW_STATE_CHARACTER_SELECT_OR_INGAME
            {
                if let Some(first_seen_at) = first_window_seen_at {
                    if first_seen_at.elapsed() >= wow_login.login_fallback_after {
                        println!(
                            "应用 [{}] 在 {}ms 内未进入明确登录态，开始执行兜底账号密码输入",
                            app.name,
                            wow_login.login_fallback_after.as_millis()
                        );
                        focus_window(hwnd);
                        let _ = send_virtual_key(VK_RETURN);
                        sleep(Duration::from_millis(800));
                        submit_wow_credentials(hwnd, wow_login, &app.name)?;
                        credentials_submitted = true;
                        account_select_confirmed = true;
                        fallback_login_attempted = true;
                        sleep(wow_login.after_login_submit_delay);
                        continue;
                    }
                }
            }

            match state {
                WOW_STATE_ACCOUNT_SELECT
                    if !account_select_confirmed && enter_world_started_at.is_none() =>
                {
                    focus_window(hwnd);
                    send_virtual_key(VK_RETURN)?;
                    account_select_confirmed = true;
                    println!("应用 [{}] 已处理账号选择界面", app.name);
                    sleep(wow_login.state_poll_interval);
                }
                WOW_STATE_LOGIN_SCREEN
                    if !credentials_submitted && enter_world_started_at.is_none() =>
                {
                    submit_wow_credentials(hwnd, wow_login, &app.name)?;
                    credentials_submitted = true;
                    sleep(wow_login.after_login_submit_delay);
                }
                WOW_STATE_CHARACTER_SELECT_OR_INGAME => {
                    if let Some(started_at) = enter_world_started_at {
                        if started_at.elapsed() >= wow_login.after_enter_world_delay {
                            println!("应用 [{}] 已完成 WoW 登录，启动器退出", app.name);
                            return Ok(());
                        }

                        sleep(wow_login.state_poll_interval);
                    } else {
                        let handle = open_process_for_wow(target_pid)?;
                        let current_index_result =
                            read_process_u32(handle, WOW_SELECTED_CHARACTER_INDEX_ADDRESS);
                        let current_index = match current_index_result {
                            Ok(value) => value,
                            Err(err) => {
                                close_handle(handle);
                                return Err(err);
                            }
                        };
                        if current_index != wow_login.character_index {
                            if let Err(err) = write_process_u32(
                                handle,
                                WOW_SELECTED_CHARACTER_INDEX_ADDRESS,
                                wow_login.character_index,
                            ) {
                                close_handle(handle);
                                return Err(err);
                            }
                        }
                        close_handle(handle);

                        focus_window(hwnd);
                        send_virtual_key(VK_RETURN)?;
                        enter_world_started_at = Some(Instant::now());
                        println!(
                            "应用 [{}] 已选择角色索引 {} 并触发进入游戏，等待登录完成",
                            app.name, wow_login.character_index
                        );
                        sleep(wow_login.state_poll_interval);
                    }
                }
                _ => {
                    sleep(wow_login.state_poll_interval);
                }
            }
        }
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

        run_wow_auto_login(app, launched_pid, &before_launch)?;

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
