//! Tauri host wiring and application logic.

mod logging;
mod paths;
mod platform;
mod loader_map;
mod rkdev;
mod usb;

use std::sync::Arc;

use tauri::Manager;

/// Application state, UI bridge, and IPC commands.
mod app {
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use serde::Serialize;
    use tauri::{AppHandle, Manager, State};
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_opener::OpenerExt;

    use super::logging;
    use super::paths;
    use super::platform::flashing;
    use super::loader_map;
    use super::rkdev::{self, ProcessResult, RkdevTask};
    use super::usb;

    pub const STORAGE_EMMC: u32 = 1;
    pub const STORAGE_SD: u32 = 2;
    pub const STORAGE_SPI_NOR: u32 = 9;

    pub struct AppState {
        pub device_present: AtomicBool,
        pub last_vid: AtomicU16,
        pub last_pid: AtomicU16,
        pub connect_requested: AtomicBool,
        pub loader_ready: AtomicBool,
        pub flash_running: AtomicBool,
        /// Set by Cancel; checked by long paths that don't hold an RkdevTask
        /// (e.g. connect when the loader is already running and only probing).
        pub cancel_requested: AtomicBool,
        pub available_storage_mask: AtomicU32,
        pub selected_storage: AtomicU32,
        pub last_storage_sectors: AtomicU64,
        pub storage_probe_complete: AtomicBool,
        pub flash_task: Mutex<Option<Arc<RkdevTask>>>,
        pub probe_mutex: Mutex<()>,
    }

    impl AppState {
        pub fn new() -> Self {
            Self {
                device_present: AtomicBool::new(false),
                last_vid: AtomicU16::new(0),
                last_pid: AtomicU16::new(0),
                connect_requested: AtomicBool::new(false),
                loader_ready: AtomicBool::new(false),
                flash_running: AtomicBool::new(false),
                cancel_requested: AtomicBool::new(false),
                available_storage_mask: AtomicU32::new(0),
                selected_storage: AtomicU32::new(0),
                last_storage_sectors: AtomicU64::new(0),
                storage_probe_complete: AtomicBool::new(false),
                flash_task: Mutex::new(None),
                probe_mutex: Mutex::new(()),
            }
        }
    }

    pub fn storage_bit(storage: u32) -> u32 {
        match storage {
            STORAGE_EMMC => 1 << 0,
            STORAGE_SD => 1 << 1,
            STORAGE_SPI_NOR => 1 << 2,
            _ => 0,
        }
    }

    pub fn storage_name(storage: u32) -> &'static str {
        match storage {
            STORAGE_EMMC => "eMMC",
            STORAGE_SD => "SD card",
            STORAGE_SPI_NOR => "SPI NOR",
            _ => "storage",
        }
    }

    pub fn is_known_storage(storage: u32) -> bool {
        matches!(storage, STORAGE_EMMC | STORAGE_SD | STORAGE_SPI_NOR)
    }

    // ----- UI bridge (webview.eval) -----


    fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
        app.get_webview_window("main")
    }

    pub fn eval(app: &AppHandle, js: &str) {
        if let Some(w) = main_window(app) {
            let _ = w.eval(js);
        }
    }

    pub fn update_device_status(app: &AppHandle, status: &str) {
        let s = serde_json::to_string(status).unwrap_or_else(|_| "\"\"".into());
        eval(app, &format!("window.updateDeviceStatus && window.updateDeviceStatus({s})"));
    }

    pub fn update_device_info(app: &AppHandle, info: &str) {
        let s = serde_json::to_string(info).unwrap_or_else(|_| "\"\"".into());
        eval(app, &format!("window.updateDeviceInfo && window.updateDeviceInfo({s})"));
    }

    pub fn update_device_soc(app: &AppHandle, soc: &str) {
        let s = serde_json::to_string(soc).unwrap_or_else(|_| "\"\"".into());
        eval(app, &format!("window.updateDeviceSoc && window.updateDeviceSoc({s})"));
    }

    pub fn update_flash_progress(app: &AppHandle, percent: i32) {
        eval(
            app,
            &format!("window.updateFlashProgress && window.updateFlashProgress({percent})"),
        );
    }

    pub fn on_flash_complete(app: &AppHandle, success: bool, cancelled: bool, error: &str) {
        let err = serde_json::to_string(error).unwrap_or_else(|_| "\"\"".into());
        eval(
            app,
            &format!(
                "window.onFlashComplete && window.onFlashComplete({{success:{success}, cancelled:{cancelled}, error:{err}}})"
            ),
        );
    }

    /// Deliver an OS file drop (single .img) to the UI, mirroring the shape the
    /// `select_image_file` command returns.
    pub fn on_image_file_dropped(app: &AppHandle, path: &str, size_bytes: u64) {
        let p = serde_json::to_string(path).unwrap_or_else(|_| "\"\"".into());
        eval(
            app,
            &format!(
                "window.onImageFileDropped && window.onImageFileDropped({{success:true, path:{p}, sizeBytes:{size_bytes}}})"
            ),
        );
    }

    /// Drive the drag-and-drop overlay. `active` shows/hides it; `valid` picks
    /// the accept vs reject styling while a drag hovers.
    pub fn on_image_drag_state(app: &AppHandle, active: bool, valid: bool) {
        eval(
            app,
            &format!(
                "window.onImageDragState && window.onImageDragState({{active:{active}, valid:{valid}}})"
            ),
        );
    }

    pub fn on_driver_install_complete(app: &AppHandle, success: bool, error: &str) {
        let err = serde_json::to_string(error).unwrap_or_else(|_| "\"\"".into());
        eval(
            app,
            &format!(
                "window.onDriverInstallComplete && window.onDriverInstallComplete({{success:{success}, cancelled:false, error:{err}}})"
            ),
        );
    }

    pub fn append_live_log(app: &AppHandle, line: &str, replace: bool) {
        let s = serde_json::to_string(line).unwrap_or_else(|_| "\"\"".into());
        eval(
            app,
            &format!("window.appendLiveLog && window.appendLiveLog({s}, {replace})"),
        );
    }

    // ----- Commands -----

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct StartResult {
        pub started: bool,
        pub error: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct BackupStartResult {
        pub started: bool,
        pub needs_confirmation: bool,
        pub message: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DependencyStatus {
        pub ok: bool,
        pub warning: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DeviceAccessInfo {
        pub kind: String,
        pub device_relevant: bool,
        pub ready: bool,
        pub detail: String,
        pub error: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct FilePickResult {
        pub success: bool,
        pub path: String,
        pub error: String,
        pub size_bytes: u64,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct StorageInfoResult {
        pub success: bool,
        pub storage_bytes: u64,
        pub error: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct StorageTargetsResult {
        pub success: bool,
        pub emmc_available: bool,
        pub sd_available: bool,
        pub spinor_available: bool,
        pub selected_storage: u32,
        pub error: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UsedSpaceResult {
        pub success: bool,
        pub used_bytes: u64,
        pub error: String,
    }

    fn parse_flash_size_sectors(rfi: &str) -> u64 {
        // Typical: "Flash Size: 30528MB" or sector counts — match C++ loosely.
        let re_mb = regex::Regex::new(r"(?i)Flash\s*Size\s*:\s*(\d+)\s*MB").ok();
        if let Some(re) = re_mb {
            if let Some(c) = re.captures(rfi) {
                if let Ok(mb) = c[1].parse::<u64>() {
                    return mb * 1024 * 1024 / 512;
                }
            }
        }
        let re_sec = regex::Regex::new(r"(?i)(\d+)\s*sectors?").ok();
        if let Some(re) = re_sec {
            if let Some(c) = re.captures(rfi) {
                if let Ok(n) = c[1].parse::<u64>() {
                    return n;
                }
            }
        }
        0
    }

    fn push_device_ui(app: &AppHandle, state: &AppState) {
        let present = state.device_present.load(Ordering::SeqCst);
        let loader = state.loader_ready.load(Ordering::SeqCst);
        let vid = state.last_vid.load(Ordering::SeqCst);
        let pid = state.last_pid.load(Ordering::SeqCst);

        // UI (app.js) only shows Connect when status is "detected" or "connected".
        // "maskrom" is not a recognized status string in the frontend.
        let status = if !rkdev::tool_available() && present {
            "tool_missing"
        } else if !present {
            "disconnected"
        } else if loader {
            "connected"
        } else {
            "detected"
        };
        update_device_status(app, status);

        let info = if present {
            format!("VID {vid:04X} PID {pid:04X}")
        } else {
            String::new()
        };
        update_device_info(app, &info);

        let soc = if present {
            loader_map::soc_name(vid, pid).unwrap_or("unknown").to_string()
        } else {
            String::new()
        };
        update_device_soc(app, &soc);
    }

    fn probe_storage_targets(state: &AppState) {
        let _guard = state.probe_mutex.lock().unwrap();
        let mut mask = 0u32;
        for storage in [STORAGE_EMMC, STORAGE_SD, STORAGE_SPI_NOR] {
            let (res, _) = rkdev::run_sync_output(&["cs", &storage.to_string()]);
            if res.exit_code == 0 {
                mask |= storage_bit(storage);
            }
        }
        state.available_storage_mask.store(mask, Ordering::SeqCst);
        // Prefer eMMC, then SD, then SPI NOR
        let selected = if mask & storage_bit(STORAGE_EMMC) != 0 {
            STORAGE_EMMC
        } else if mask & storage_bit(STORAGE_SD) != 0 {
            STORAGE_SD
        } else if mask & storage_bit(STORAGE_SPI_NOR) != 0 {
            STORAGE_SPI_NOR
        } else {
            0
        };
        if selected != 0 {
            let _ = rkdev::run_sync_output(&["cs", &selected.to_string()]);
            state.selected_storage.store(selected, Ordering::SeqCst);
            // SD capacity from rfi is unreliable — never cache/display it.
            if selected == STORAGE_SD {
                state.last_storage_sectors.store(0, Ordering::SeqCst);
            } else {
                let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
                let sectors = parse_flash_size_sectors(&rfi);
                state.last_storage_sectors.store(sectors, Ordering::SeqCst);
            }
        }
        state.storage_probe_complete.store(true, Ordering::SeqCst);
    }

    fn start_flash_task(
        app: AppHandle,
        state: Arc<AppState>,
        args: Vec<String>,
        on_success_connect: bool,
        on_success_disconnect: bool,
        cleanup: Option<Box<dyn FnOnce() + Send>>,
    ) -> bool {
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        state.cancel_requested.store(false, Ordering::SeqCst);

        update_flash_progress(&app, 0);
        let last_percent = Arc::new(Mutex::new(-1i32));
        let app_line = app.clone();
        let on_line = move |line: String| {
            if let Some(p) = rkdev::parse_progress_percent(&line) {
                let mut last = last_percent.lock().unwrap();
                if p != *last {
                    *last = p;
                    update_flash_progress(&app_line, p);
                }
            }
        };

        let app_exit = app.clone();
        let state_exit = state.clone();
        let cleanup = Mutex::new(cleanup);

        let task = match rkdev::start(
            args,
            on_line,
            move |result: ProcessResult| {
                *state_exit.flash_task.lock().unwrap() = None;
                if let Ok(mut c) = cleanup.lock() {
                    if let Some(f) = c.take() {
                        f();
                    }
                }

                let cancelled = result.was_cancelled;
                let success =
                    result.exit_code == 0 && result.error_message.is_empty() && !cancelled;

                if success && on_success_connect {
                    let (_, chip) = rkdev::run_sync_output(&["rci"]);
                    for line in chip.lines() {
                        if line.to_lowercase().contains("chip") {
                            logging::write_line(&format!("[app] {line}"));
                        }
                    }
                    probe_storage_targets(&state_exit);
                    state_exit.loader_ready.store(true, Ordering::SeqCst);
                    push_device_ui(&app_exit, &state_exit);
                } else if success && on_success_disconnect {
                    state_exit.connect_requested.store(false, Ordering::SeqCst);
                    state_exit.loader_ready.store(false, Ordering::SeqCst);
                    state_exit.available_storage_mask.store(0, Ordering::SeqCst);
                    state_exit.selected_storage.store(0, Ordering::SeqCst);
                    state_exit.last_storage_sectors.store(0, Ordering::SeqCst);
                    state_exit
                        .storage_probe_complete
                        .store(false, Ordering::SeqCst);
                    push_device_ui(&app_exit, &state_exit);
                }

                state_exit.flash_running.store(false, Ordering::SeqCst);
                if success {
                    update_flash_progress(&app_exit, 100);
                }
                let err = if !success && !cancelled {
                    if result.error_message.is_empty() {
                        format!("rkdeveloptool failed with exit code {}", result.exit_code)
                    } else {
                        result.error_message
                    }
                } else {
                    String::new()
                };
                on_flash_complete(&app_exit, success, cancelled, &err);
            },
        ) {
            Ok(t) => t,
            Err(e) => {
                state.flash_running.store(false, Ordering::SeqCst);
                on_flash_complete(&app, false, false, &e);
                return false;
            }
        };

        *state.flash_task.lock().unwrap() = Some(task);
        true
    }

    #[tauri::command]
    pub fn get_platform() -> String {
        paths::platform_name().to_string()
    }

    #[tauri::command]
    pub fn get_dependency_status() -> DependencyStatus {
        match paths::rkdeveloptool_path() {
            Ok(_) => DependencyStatus {
                ok: true,
                warning: String::new(),
            },
            Err(msg) => DependencyStatus {
                ok: false,
                warning: msg,
            },
        }
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct LogContentsResult {
        pub ok: bool,
        pub text: String,
    }

    #[tauri::command]
    pub fn get_log_contents() -> LogContentsResult {
        LogContentsResult {
            ok: true,
            text: logging::read_all(),
        }
    }

    #[tauri::command]
    pub fn open_log_directory(app: AppHandle) -> Result<(), String> {
        let dir = logging::log_directory();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        app.opener()
            .open_path(dir.to_string_lossy(), None::<&str>)
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub fn ui_ready(app: AppHandle, state: State<'_, Arc<AppState>>) -> bool {
        logging::write_line("[app] ui_ready");
        let app_c = app.clone();
        let state_c = state.inner().clone();

        let on_usb = Arc::new(move |present: bool, vid: u16, pid: u16| {
            state_c.device_present.store(present, Ordering::SeqCst);
            if present {
                state_c.last_vid.store(vid, Ordering::SeqCst);
                state_c.last_pid.store(pid, Ordering::SeqCst);
            } else {
                state_c.loader_ready.store(false, Ordering::SeqCst);
                state_c.connect_requested.store(false, Ordering::SeqCst);
                state_c.available_storage_mask.store(0, Ordering::SeqCst);
                state_c.selected_storage.store(0, Ordering::SeqCst);
                state_c.last_storage_sectors.store(0, Ordering::SeqCst);
                state_c
                    .storage_probe_complete
                    .store(false, Ordering::SeqCst);
            }
            push_device_ui(&app_c, &state_c);
        });

        let _ = usb::start(on_usb);
        push_device_ui(&app, state.inner());
        true
    }

    #[tauri::command]
    pub fn get_device_access_info() -> DeviceAccessInfo {
        let s = flashing::query();
        DeviceAccessInfo {
            kind: s.kind.as_str().to_string(),
            device_relevant: s.device_relevant,
            ready: s.ready,
            detail: s.detail,
            error: s.error,
        }
    }

    #[tauri::command]
    pub fn install_device_access(app: AppHandle, device_name: Option<String>) -> StartResult {
        let name = device_name.unwrap_or_default();
        std::thread::spawn(move || {
            let opts = flashing::InstallOptions {
                device_name: name,
            };
            let result = flashing::install(&opts);
            on_driver_install_complete(&app, result.success, &result.error_message);
        });
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    /// Must be `async` and use the *blocking* dialog APIs so the picker does not
    /// run on the main event-loop thread. On macOS, callback + `recv()` on a
    /// sync command deadlocks (panel needs the main thread, which is blocked).
    #[tauri::command]
    pub async fn select_image_file(app: AppHandle) -> FilePickResult {
        let file_path = app
            .dialog()
            .file()
            .add_filter("Disk Images", &["img"])
            .set_title("Select .img file")
            .blocking_pick_file();

        match file_path {
            Some(file_path) => {
                let path = match file_path.as_path() {
                    Some(p) => p.to_string_lossy().into_owned(),
                    None => file_path.to_string(),
                };
                let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                FilePickResult {
                    success: true,
                    path,
                    error: String::new(),
                    size_bytes: size,
                }
            }
            None => FilePickResult {
                success: false,
                path: String::new(),
                error: "file picker canceled".into(),
                size_bytes: 0,
            },
        }
    }

    #[tauri::command]
    pub async fn select_backup_destination(app: AppHandle) -> FilePickResult {
        let file_path = app
            .dialog()
            .file()
            .add_filter("Disk Images", &["img"])
            .set_title("Save storage backup as")
            .set_file_name("backup.img")
            .blocking_save_file();

        match file_path {
            Some(file_path) => {
                let mut path = match file_path.as_path() {
                    Some(p) => p.to_string_lossy().into_owned(),
                    None => file_path.to_string(),
                };
                if !path.ends_with(".img") {
                    path.push_str(".img");
                }
                FilePickResult {
                    success: true,
                    path,
                    error: String::new(),
                    size_bytes: 0,
                }
            }
            None => FilePickResult {
                success: false,
                path: String::new(),
                error: "file picker canceled".into(),
                size_bytes: 0,
            },
        }
    }

    #[tauri::command]
    pub fn flash_bootloader(app: AppHandle, state: State<'_, Arc<AppState>>) -> StartResult {
        if !state.device_present.load(Ordering::SeqCst) {
            return StartResult {
                started: false,
                error: "no device present".into(),
            };
        }
        // Claim the device for the whole Connect path (td probe and optional db).
        // Must not run long rkdeveloptool work on this invoke thread — when the
        // loader is already up, `cs`/`rfi` can block and froze the UI (no timeout
        // previously). All probes run on a worker with PROBE_TIMEOUT.
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }

        state.connect_requested.store(true, Ordering::SeqCst);
        state.cancel_requested.store(false, Ordering::SeqCst);
        let app = app.clone();
        let state = state.inner().clone();

        std::thread::spawn(move || {
            let already_ready = {
                let _g = state.probe_mutex.lock().unwrap();
                let (res, _) = rkdev::run_sync_output(&["td"]);
                res.exit_code == 0
            };

            // Complete as cancelled only if we still hold the flash_running claim
            // (cancel_flash may already have unlocked the UI when no rkdev task).
            let finish_if_cancelled = |app: &AppHandle, state: &AppState, keep_loader: bool| -> bool {
                if !state.cancel_requested.swap(false, Ordering::SeqCst) {
                    return false;
                }
                let still_claimed = state.flash_running.swap(false, Ordering::SeqCst);
                if keep_loader {
                    state.loader_ready.store(true, Ordering::SeqCst);
                    push_device_ui(app, state);
                } else {
                    state.connect_requested.store(false, Ordering::SeqCst);
                }
                if still_claimed {
                    on_flash_complete(app, false, true, "");
                }
                true
            };

            if finish_if_cancelled(&app, &state, false) {
                return;
            }

            if already_ready {
                logging::write_line("[app] Connect: loader already running");
                {
                    let _g = state.probe_mutex.lock().unwrap();
                    let (_, chip) = rkdev::run_sync_output(&["rci"]);
                    for line in chip.lines() {
                        if line.to_lowercase().contains("chip") {
                            logging::write_line(&format!("[app] {line}"));
                        }
                    }
                    // probe_storage_targets takes probe_mutex itself
                }
                if finish_if_cancelled(&app, &state, false) {
                    return;
                }
                probe_storage_targets(&state);
                if finish_if_cancelled(&app, &state, true) {
                    return;
                }
                state.flash_running.store(false, Ordering::SeqCst);
                state.loader_ready.store(true, Ordering::SeqCst);
                push_device_ui(&app, &state);
                update_flash_progress(&app, 100);
                on_flash_complete(&app, true, false, "");
                return;
            }

            // Maskrom: need SPL download. Check cancel before releasing the
            // claim so start_flash_task can take it for `db`.
            if finish_if_cancelled(&app, &state, false) {
                return;
            }
            state.flash_running.store(false, Ordering::SeqCst);

            let vid = state.last_vid.load(Ordering::SeqCst);
            let pid = state.last_pid.load(Ordering::SeqCst);
            let Some(entry) = loader_map::entry_for(vid, pid) else {
                state.connect_requested.store(false, Ordering::SeqCst);
                on_flash_complete(
                    &app,
                    false,
                    false,
                    &format!(
                        "unrecognized device (VID 0x{vid:04X} PID 0x{pid:04X}) - not a supported Rockchip SoC"
                    ),
                );
                return;
            };
            let Some(filename) = entry.filename else {
                state.connect_requested.store(false, Ordering::SeqCst);
                on_flash_complete(
                    &app,
                    false,
                    false,
                    &format!(
                        "{} is not supported - no loader is available for this SoC",
                        entry.soc
                    ),
                );
                return;
            };
            let Some(loader) = paths::loader_path(filename) else {
                state.connect_requested.store(false, Ordering::SeqCst);
                on_flash_complete(
                    &app,
                    false,
                    false,
                    &format!("loader file not found: {filename}"),
                );
                return;
            };

            logging::write_line(&format!("[app] Connect: download boot {}", loader.display()));
            if !start_flash_task(
                app.clone(),
                state.clone(),
                vec!["db".into(), loader.to_string_lossy().into_owned()],
                true,
                false,
                None,
            ) {
                state.connect_requested.store(false, Ordering::SeqCst);
                on_flash_complete(&app, false, false, "flash already in progress");
            }
        });

        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn disconnect_device(app: AppHandle, state: State<'_, Arc<AppState>>) -> StartResult {
        if !state.loader_ready.load(Ordering::SeqCst) {
            return StartResult {
                started: false,
                error: "device is not connected".into(),
            };
        }
        logging::write_line("[app] Disconnect: resetting device to maskrom");
        // `rd 3` = RST_RESETMASKROM_SUBCODE: reset back into maskrom rather than
        // a plain `rd` (subcode 0), which would reboot into normal flash boot.
        if !start_flash_task(
            app,
            state.inner().clone(),
            vec!["rd".into(), "3".into()],
            false,
            true,
            None,
        ) {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn flash_image(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        image_path: String,
    ) -> StartResult {
        if image_path.is_empty() {
            return StartResult {
                started: false,
                error: "no .img file selected".into(),
            };
        }
        let path = PathBuf::from(&image_path);
        if path.extension().and_then(|e| e.to_str()) != Some("img") {
            return StartResult {
                started: false,
                error: "selected file is not a .img".into(),
            };
        }
        if !path.is_file() {
            return StartResult {
                started: false,
                error: "selected file does not exist".into(),
            };
        }
        logging::write_line(&format!("[app] Flash Image: {image_path}"));
        if !start_flash_task(
            app,
            state.inner().clone(),
            vec!["wl".into(), "0".into(), image_path],
            false,
            false,
            None,
        ) {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn erase_storage(app: AppHandle, state: State<'_, Arc<AppState>>) -> StartResult {
        logging::write_line("[app] Quick Erase");
        if !start_flash_task(
            app,
            state.inner().clone(),
            vec!["ef".into()],
            false,
            false,
            None,
        ) {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn secure_erase_storage(app: AppHandle, state: State<'_, Arc<AppState>>) -> StartResult {
        let storage = state.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return StartResult {
                started: false,
                error: "no storage target selected".into(),
            };
        }
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        let mut total_sectors = {
            let _g = state.probe_mutex.lock().unwrap();
            let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
            parse_flash_size_sectors(&rfi)
        };
        state.flash_running.store(false, Ordering::SeqCst);
        let cached = state.last_storage_sectors.load(Ordering::SeqCst);
        if cached != 0 {
            total_sectors = cached;
        }
        if total_sectors == 0 {
            return StartResult {
                started: false,
                error: format!(
                    "could not determine {} size",
                    storage_name(storage)
                ),
            };
        }

        let zero_path = std::env::temp_dir().join("rui_secure_erase_storage_zero.img");
        let _ = fs::remove_file(&zero_path);
        if File::create(&zero_path).is_err() {
            return StartResult {
                started: false,
                error: "failed to prepare erase source file".into(),
            };
        }
        // Sparse zero file of full capacity (reads as zeros).
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let f = File::options().write(true).open(&zero_path).ok();
            if let Some(f) = f {
                let _ = f.write_at(&[0u8], total_sectors * 512 - 1);
            }
        }
        #[cfg(not(unix))]
        {
            if fs::File::options()
                .write(true)
                .open(&zero_path)
                .and_then(|f| f.set_len(total_sectors * 512))
                .is_err()
            {
                return StartResult {
                    started: false,
                    error: "failed to prepare erase source file".into(),
                };
            }
        }

        logging::write_line(&format!(
            "[app] Secure Erase: overwriting {} bytes with zeros",
            total_sectors * 512
        ));
        let zp = zero_path.clone();
        if !start_flash_task(
            app,
            state.inner().clone(),
            vec![
                "wl".into(),
                "0".into(),
                zero_path.to_string_lossy().into_owned(),
            ],
            false,
            false,
            Some(Box::new(move || {
                let _ = fs::remove_file(&zp);
            })),
        ) {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn backup_storage(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        dest_path: String,
        force: bool,
    ) -> BackupStartResult {
        if dest_path.is_empty() {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "no destination selected".into(),
            };
        }
        let storage = state.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "no storage target selected".into(),
            };
        }
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "flash already in progress".into(),
            };
        }

        let (mut main_sectors, mut total_sectors) = {
            let _g = state.probe_mutex.lock().unwrap();
            let main = rkdev::read_gpt_info().map(|g| g.last_used_lba + 1).unwrap_or(0);
            let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
            let total = parse_flash_size_sectors(&rfi);
            (main, total)
        };
        state.flash_running.store(false, Ordering::SeqCst);
        let cached = state.last_storage_sectors.load(Ordering::SeqCst);
        if cached != 0 {
            total_sectors = cached;
        }

        if main_sectors == 0 {
            if total_sectors == 0 {
                return BackupStartResult {
                    started: false,
                    needs_confirmation: false,
                    message: format!(
                        "could not determine {} size",
                        storage_name(storage)
                    ),
                };
            }
            if !force {
                let total_gb = total_sectors as f64 * 512.0 / (1024.0 * 1024.0 * 1024.0);
                return BackupStartResult {
                    started: false,
                    needs_confirmation: true,
                    message: format!(
                        "No partition table was found on this storage target, so it can't be trimmed precisely. \
                         If this device was previously flashed and erased, its old data may still be physically \
                         present and could be captured in this backup (erase does not guarantee a secure wipe). \
                         This will back up the entire {total_gb:.1} GiB device. Continue?"
                    ),
                };
            }
            main_sectors = total_sectors;
        }

        logging::write_line(&format!(
            "[app] Backup {}: {main_sectors} sectors -> {dest_path}",
            storage_name(storage)
        ));
        if !start_flash_task(
            app,
            state.inner().clone(),
            vec![
                "rl".into(),
                "0".into(),
                main_sectors.to_string(),
                dest_path,
            ],
            false,
            false,
            None,
        ) {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "flash already in progress".into(),
            };
        }
        BackupStartResult {
            started: true,
            needs_confirmation: false,
            message: String::new(),
        }
    }

    #[tauri::command]
    pub fn cancel_flash(app: AppHandle, state: State<'_, Arc<AppState>>) -> StartResult {
        logging::write_line("[app] Cancel requested");
        state.cancel_requested.store(true, Ordering::SeqCst);

        let had_task = {
            let guard = state.flash_task.lock().unwrap();
            if let Some(task) = guard.as_ref() {
                task.cancel();
                true
            } else {
                false
            }
        };

        if had_task {
            // on_exit of the rkdev task will call on_flash_complete(cancelled).
            return StartResult {
                started: true,
                error: String::new(),
            };
        }

        // No live rkdeveloptool process (connect probe-only path, or a stuck
        // UI claim). Unlock immediately so Cancel always ends the operation.
        if state.flash_running.swap(false, Ordering::SeqCst) {
            logging::write_line("[app] Cancel: no rkdev task — unlocking UI");
            on_flash_complete(&app, false, true, "");
            StartResult {
                started: true,
                error: String::new(),
            }
        } else {
            StartResult {
                started: false,
                error: "no flash in progress".into(),
            }
        }
    }

    #[tauri::command]
    pub fn force_close_window(app: AppHandle, state: State<'_, Arc<AppState>>) -> bool {
        if let Some(task) = state.flash_task.lock().unwrap().as_ref() {
            task.cancel();
        }
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.close();
        }
        true
    }

    #[tauri::command]
    pub fn get_storage_info(state: State<'_, Arc<AppState>>) -> StorageInfoResult {
        let storage = state.selected_storage.load(Ordering::SeqCst);
        if storage == 0 || !state.loader_ready.load(Ordering::SeqCst) {
            return StorageInfoResult {
                success: false,
                storage_bytes: 0,
                error: "no storage selected".into(),
            };
        }
        // rkdeveloptool's flash-info size for SD is not trustworthy (often
        // reports the eMMC geometry or a nonsense value). Always surface as
        // unknown in the UI rather than a misleading capacity.
        if storage == STORAGE_SD {
            return StorageInfoResult {
                success: false,
                storage_bytes: 0,
                error: String::new(),
            };
        }
        let mut sectors = state.last_storage_sectors.load(Ordering::SeqCst);
        if sectors == 0 {
            let _g = state.probe_mutex.lock().unwrap();
            // Ensure we're reading the currently selected target.
            let (cs, _) = rkdev::run_sync_output(&["cs", &storage.to_string()]);
            if cs.exit_code != 0 {
                return StorageInfoResult {
                    success: false,
                    storage_bytes: 0,
                    error: format!("could not select {}", storage_name(storage)),
                };
            }
            let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
            sectors = parse_flash_size_sectors(&rfi);
            state.last_storage_sectors.store(sectors, Ordering::SeqCst);
        }
        if sectors == 0 {
            StorageInfoResult {
                success: false,
                storage_bytes: 0,
                error: format!("could not read {} size", storage_name(storage)),
            }
        } else {
            StorageInfoResult {
                success: true,
                storage_bytes: sectors * 512,
                error: String::new(),
            }
        }
    }

    #[tauri::command]
    pub fn get_storage_targets(state: State<'_, Arc<AppState>>) -> StorageTargetsResult {
        let mask = state.available_storage_mask.load(Ordering::SeqCst);
        StorageTargetsResult {
            success: state.loader_ready.load(Ordering::SeqCst),
            emmc_available: mask & storage_bit(STORAGE_EMMC) != 0,
            sd_available: mask & storage_bit(STORAGE_SD) != 0,
            spinor_available: mask & storage_bit(STORAGE_SPI_NOR) != 0,
            selected_storage: state.selected_storage.load(Ordering::SeqCst),
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn select_storage(state: State<'_, Arc<AppState>>, storage: u32) -> StartResult {
        if !state.loader_ready.load(Ordering::SeqCst) {
            return StartResult {
                started: false,
                error: "device is not connected".into(),
            };
        }
        if !is_known_storage(storage) {
            return StartResult {
                started: false,
                error: "unknown storage target".into(),
            };
        }
        let mask = state.available_storage_mask.load(Ordering::SeqCst);
        if mask & storage_bit(storage) == 0 {
            return StartResult {
                started: false,
                error: format!("{} not detected", storage_name(storage)),
            };
        }
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "flash already in progress".into(),
            };
        }
        let _g = state.probe_mutex.lock().unwrap();
        let (res, _) = rkdev::run_sync_output(&["cs", &storage.to_string()]);
        state.flash_running.store(false, Ordering::SeqCst);
        if res.exit_code != 0 {
            return StartResult {
                started: false,
                error: format!("{} not detected", storage_name(storage)),
            };
        }
        state.selected_storage.store(storage, Ordering::SeqCst);
        logging::write_line(&format!(
            "[app] Storage selected: {}",
            storage_name(storage)
        ));
        // Never cache an SD size (rfi is unreliable there). Always refresh
        // capacity when switching to eMMC / SPI NOR.
        if storage == STORAGE_SD {
            state.last_storage_sectors.store(0, Ordering::SeqCst);
        } else {
            let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
            let sectors = parse_flash_size_sectors(&rfi);
            state.last_storage_sectors.store(sectors, Ordering::SeqCst);
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn calculate_used_space(state: State<'_, Arc<AppState>>) -> UsedSpaceResult {
        if !state.loader_ready.load(Ordering::SeqCst) {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "device is not connected".into(),
            };
        }
        let storage = state.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "no storage target selected".into(),
            };
        }
        if state
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "operation in progress".into(),
            };
        }

        let label = storage_name(storage);
        logging::write_line(&format!("[app] Calculate Used Space: {label}"));

        let result = (|| {
            let _g = state.probe_mutex.lock().unwrap();
            // Always target the currently selected device — rl probes hit
            // whatever cs last selected on the loader.
            let (cs, _) = rkdev::run_sync_output(&["cs", &storage.to_string()]);
            if cs.exit_code != 0 {
                return UsedSpaceResult {
                    success: false,
                    used_bytes: 0,
                    error: format!("{label} not detected"),
                };
            }

            // SD capacity is unreliable via rfi. Prefer GPT extent; fall back
            // to a binary-search only when rfi happens to return a size.
            if storage == STORAGE_SD {
                if let Some(gpt) = rkdev::read_gpt_info() {
                    let used = gpt.last_used_lba.saturating_add(1);
                    let used_bytes = used * 512;
                    logging::write_line(&format!(
                        "[app] Calculate Used Space ({label}, GPT): {used_bytes} bytes"
                    ));
                    return UsedSpaceResult {
                        success: true,
                        used_bytes,
                        error: String::new(),
                    };
                }
                let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
                let total = parse_flash_size_sectors(&rfi);
                if total == 0 {
                    return UsedSpaceResult {
                        success: false,
                        used_bytes: 0,
                        error: format!("could not determine used space on {label}"),
                    };
                }
                let used = rkdev::find_used_sector_boundary(total);
                let used_bytes = used * 512;
                logging::write_line(&format!(
                    "[app] Calculate Used Space ({label}): {used_bytes} bytes"
                ));
                return UsedSpaceResult {
                    success: true,
                    used_bytes,
                    error: String::new(),
                };
            }

            let mut total = state.last_storage_sectors.load(Ordering::SeqCst);
            if total == 0 {
                let (_, rfi) = rkdev::run_sync_output(&["rfi"]);
                total = parse_flash_size_sectors(&rfi);
                if total != 0 {
                    state.last_storage_sectors.store(total, Ordering::SeqCst);
                }
            }
            if total == 0 {
                return UsedSpaceResult {
                    success: false,
                    used_bytes: 0,
                    error: format!("could not read {label} size"),
                };
            }
            let used = rkdev::find_used_sector_boundary(total);
            let used_bytes = used * 512;
            logging::write_line(&format!(
                "[app] Calculate Used Space ({label}): {used_bytes} bytes"
            ));
            UsedSpaceResult {
                success: true,
                used_bytes,
                error: String::new(),
            }
        })();

        state.flash_running.store(false, Ordering::SeqCst);
        result
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();

    let app_state = Arc::new(app::AppState::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            app::get_platform,
            app::get_dependency_status,
            app::get_log_contents,
            app::open_log_directory,
            app::ui_ready,
            app::get_device_access_info,
            app::install_device_access,
            app::select_image_file,
            app::select_backup_destination,
            app::flash_bootloader,
            app::disconnect_device,
            app::flash_image,
            app::erase_storage,
            app::secure_erase_storage,
            app::backup_storage,
            app::cancel_flash,
            app::force_close_window,
            app::get_storage_info,
            app::get_storage_targets,
            app::select_storage,
            app::calculate_used_space,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            logging::set_ui_sink(move |line, replace| {
                app::append_live_log(&handle, &line, replace);
            });
            tracing::info!(target: "app", "launched");
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Destroyed => {
                usb::stop();
                let _ = window;
            }
            // Native OS file drop. HTML5 drag/drop events do not fire in the
            // webview while Tauri's native drag-drop is enabled, so the hover
            // overlay must also be driven from here.
            tauri::WindowEvent::DragDrop(dnd) => {
                // A single .img is the only thing the drop handler will accept.
                let accepts = |paths: &[std::path::PathBuf]| {
                    matches!(paths, [p] if p
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("img")))
                };
                match dnd {
                    tauri::DragDropEvent::Enter { paths, .. } => {
                        app::on_image_drag_state(window.app_handle(), true, accepts(paths));
                    }
                    tauri::DragDropEvent::Leave => {
                        app::on_image_drag_state(window.app_handle(), false, false);
                    }
                    tauri::DragDropEvent::Drop { paths, .. } => {
                        // Always clear the overlay, even for a rejected drop.
                        app::on_image_drag_state(window.app_handle(), false, false);
                        if accepts(paths) {
                            let path = &paths[0];
                            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                            app::on_image_file_dropped(
                                window.app_handle(),
                                &path.to_string_lossy(),
                                size,
                            );
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
