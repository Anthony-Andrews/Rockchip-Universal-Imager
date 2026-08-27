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
    use std::collections::HashMap;
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, Ordering};
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

    /// All state for one attached device. Every operation is scoped to a
    /// DeviceState and runs on its own thread + its own `rkdeveloptool -l <loc>`
    /// process, so devices operate fully independently and concurrently.
    pub struct DeviceState {
        pub location: u32,
        pub vid: AtomicU16,
        pub pid: AtomicU16,
        pub mode: Mutex<String>, // "Maskrom" | "Loader" | "Unknown"
        /// Raw `rfi` (flash info) output, captured once when the device connects.
        /// Shown as the device title's hover tooltip.
        pub flash_info: Mutex<String>,
        pub loader_ready: AtomicBool,
        pub flash_running: AtomicBool,
        /// Set by Cancel; checked by long paths that don't hold an RkdevTask.
        pub cancel_requested: AtomicBool,
        pub available_storage_mask: AtomicU32,
        pub selected_storage: AtomicU32,
        pub last_storage_sectors: AtomicU64,
        pub storage_probe_complete: AtomicBool,
        pub flash_task: Mutex<Option<Arc<RkdevTask>>>,
        pub probe_mutex: Mutex<()>,
        /// Live progress (0-100) of the current op; -1 when idle.
        pub progress: AtomicI32,
        /// Human label of the current op: "" | "connect" | "flash" | "erase" | ...
        pub current_op: Mutex<String>,
        /// Consecutive enumerations this device was absent (debounces removal so a
        /// device that vanishes transiently during its own reset isn't dropped).
        /// Only used by the Windows poll path; macOS/Linux are event-driven.
        #[cfg_attr(not(windows), allow(dead_code))]
        pub missed: AtomicU32,
    }

    impl DeviceState {
        fn new(location: u32) -> Self {
            Self {
                location,
                vid: AtomicU16::new(0),
                pid: AtomicU16::new(0),
                mode: Mutex::new(String::new()),
                flash_info: Mutex::new(String::new()),
                loader_ready: AtomicBool::new(false),
                flash_running: AtomicBool::new(false),
                cancel_requested: AtomicBool::new(false),
                available_storage_mask: AtomicU32::new(0),
                selected_storage: AtomicU32::new(0),
                last_storage_sectors: AtomicU64::new(0),
                storage_probe_complete: AtomicBool::new(false),
                flash_task: Mutex::new(None),
                probe_mutex: Mutex::new(()),
                progress: AtomicI32::new(-1),
                current_op: Mutex::new(String::new()),
                missed: AtomicU32::new(0),
            }
        }

        /// Clear per-device operating state (loader up, storage targets, op).
        fn reset_op_state(&self) {
            self.loader_ready.store(false, Ordering::SeqCst);
            self.cancel_requested.store(false, Ordering::SeqCst);
            self.available_storage_mask.store(0, Ordering::SeqCst);
            self.selected_storage.store(0, Ordering::SeqCst);
            self.last_storage_sectors.store(0, Ordering::SeqCst);
            self.storage_probe_complete.store(false, Ordering::SeqCst);
            self.progress.store(-1, Ordering::SeqCst);
            *self.current_op.lock().unwrap() = String::new();
            *self.flash_info.lock().unwrap() = String::new();
        }

        fn to_entry(&self) -> DeviceEntry {
            let vid = self.vid.load(Ordering::SeqCst);
            let pid = self.pid.load(Ordering::SeqCst);
            let entry = loader_map::entry_for(vid, pid);
            let mask = self.available_storage_mask.load(Ordering::SeqCst);
            DeviceEntry {
                location: self.location,
                location_hex: format!("0x{:x}", self.location),
                vid,
                pid,
                soc: entry.map(|e| e.soc).unwrap_or("unknown").to_string(),
                mode: self.mode.lock().unwrap().clone(),
                supported: entry.map(|e| e.filename.is_some()).unwrap_or(false),
                loader_ready: self.loader_ready.load(Ordering::SeqCst),
                running: self.flash_running.load(Ordering::SeqCst),
                progress: self.progress.load(Ordering::SeqCst),
                current_op: self.current_op.lock().unwrap().clone(),
                storage_mask: mask,
                selected_storage: self.selected_storage.load(Ordering::SeqCst),
                flash_info: self.flash_info.lock().unwrap().clone(),
            }
        }
    }

    pub struct AppState {
        /// All known devices, keyed by USB LocationID.
        pub devices: Mutex<HashMap<u32, Arc<DeviceState>>>,
        /// Count of in-flight operations across all devices. While > 0 the
        /// enumerator adds new devices but never removes ones (they vanish
        /// transiently during a device's own db re-enumeration).
        pub active_ops: AtomicU32,
        /// Signature of the last-pushed physical device set, so re-enumeration
        /// only pushes to the UI on an actual change.
        pub last_device_sig: Mutex<String>,
        /// Serializes `ld` enumeration. Concurrent `ld` processes contend on USB
        /// enumeration and hang (then get killed at the probe timeout, which
        /// disrupts the bus); only ever run one at a time.
        pub enum_mutex: Mutex<()>,
        /// Close-cleanup latch: set once the on-quit maskrom reset has started,
        /// so re-entrant CloseRequested events don't start it twice.
        pub cleanup_started: AtomicBool,
        /// Set when close cleanup has finished; the window may actually close.
        pub close_ready: AtomicBool,
    }

    impl AppState {
        pub fn new() -> Self {
            Self {
                devices: Mutex::new(HashMap::new()),
                active_ops: AtomicU32::new(0),
                last_device_sig: Mutex::new(String::new()),
                enum_mutex: Mutex::new(()),
                cleanup_started: AtomicBool::new(false),
                close_ready: AtomicBool::new(false),
            }
        }
    }

    /// Look up a device's state by LocationID (None if it is gone).
    fn get_device(state: &AppState, location: u32) -> Option<Arc<DeviceState>> {
        state.devices.lock().unwrap().get(&location).cloned()
    }

    /// Marks the app as doing USB work so the background enumeration poll pauses
    /// (its in-process `get_device_list` contends with child rkdeveloptool device
    /// opens on macOS → "Creating Comm Object failed"). Bumps `active_ops`, then
    /// drains any in-flight enumeration by taking `enum_mutex` — so once this
    /// exists, no enumeration can be running or start. Decrements on drop.
    ///
    /// Ops on different devices can hold this concurrently (parallel flashing);
    /// it only excludes the enumerator, not other ops.
    struct BusyGuard<'a> {
        state: &'a AppState,
    }

    impl<'a> BusyGuard<'a> {
        fn new(state: &'a AppState) -> Self {
            state.active_ops.fetch_add(1, Ordering::SeqCst);
            drop(state.enum_mutex.lock().unwrap()); // wait out any running enumeration
            BusyGuard { state }
        }
    }

    impl Drop for BusyGuard<'_> {
        fn drop(&mut self) {
            self.state.active_ops.fetch_sub(1, Ordering::SeqCst);
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

    /// Push the full device list (each entry carries its own live op state) so
    /// the UI can render every row's controls + progress.
    pub fn update_device_list(app: &AppHandle, devices: &[DeviceEntry]) {
        let json = serde_json::to_string(devices).unwrap_or_else(|_| "[]".into());
        eval(app, &format!("window.updateDeviceList && window.updateDeviceList({json})"));
    }

    /// Lightweight progress tick for one device (0-100). Frequent, so it patches
    /// a single row rather than re-pushing the whole list.
    pub fn on_device_progress(app: &AppHandle, location: u32, percent: i32) {
        eval(
            app,
            &format!("window.onDeviceProgress && window.onDeviceProgress({location}, {percent})"),
        );
    }

    /// One device's operation finished (success / cancelled / error). `stats`
    /// is a human-readable size/time/speed summary for successful streaming
    /// ops ("" when not applicable); the UI appends it to the success message.
    pub fn on_device_op_complete(
        app: &AppHandle,
        location: u32,
        op: &str,
        success: bool,
        cancelled: bool,
        error: &str,
        stats: &str,
    ) {
        let o = serde_json::to_string(op).unwrap_or_else(|_| "\"\"".into());
        let err = serde_json::to_string(error).unwrap_or_else(|_| "\"\"".into());
        let st = serde_json::to_string(stats).unwrap_or_else(|_| "\"\"".into());
        eval(
            app,
            &format!(
                "window.onDeviceOpComplete && window.onDeviceOpComplete({{location:{location}, op:{o}, success:{success}, cancelled:{cancelled}, error:{err}, stats:{st}}})"
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

    /// One attached rockusb device plus its live operating state, for the UI
    /// device list (each row renders its own controls + progress bar).
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DeviceEntry {
        pub location: u32,
        pub location_hex: String,
        pub vid: u16,
        pub pid: u16,
        pub soc: String,
        pub mode: String,
        pub supported: bool,
        pub loader_ready: bool,
        pub running: bool,
        pub progress: i32,
        pub current_op: String,
        pub storage_mask: u32,
        pub selected_storage: u32,
        /// Raw `rfi` flash-info text for the hover tooltip ("" until connected).
        pub flash_info: String,
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

    /// Build the current device list from the state map and push it to the UI.
    fn push_device_list(app: &AppHandle, state: &AppState) {
        let mut entries: Vec<DeviceEntry> = {
            let map = state.devices.lock().unwrap();
            map.values().map(|d| d.to_entry()).collect()
        };
        entries.sort_by_key(|e| e.location);
        update_device_list(app, &entries);
    }

    /// Re-enumerate attached devices (`ld`) and reconcile the state map: add new
    /// devices, refresh modes, drop unplugged ones (cancelling any in-flight op),
    /// then push the list.
    ///
    /// `ld` only enumerates the bus (it never opens a device), so it is safe to
    /// run alongside an in-flight db/wl. The one hazard is removal: during `db`
    /// the target briefly drops off the bus (maskrom→loader re-enumeration), so
    /// while any op is running we ADD newly-seen devices but never REMOVE ones
    /// (that would cancel the very operation causing the transient). Full
    /// reconcile (including removals) only happens when idle.
    ///
    /// Pushes to the UI only when the physical device set changed, so the
    /// safety-net poll doesn't churn open dropdowns.
    ///
    /// Windows only: macOS/Linux are event-driven (see apply_device_event).
    #[cfg(windows)]
    fn emit_device_list(app: &AppHandle, state: &AppState) {
        // Serialize enumeration so concurrent triggers don't pile up.
        let Ok(_enum_guard) = state.enum_mutex.try_lock() else {
            return;
        };
        // Never enumerate while an operation is running: the in-process
        // get_device_list contends with child rkdeveloptool device opens on macOS
        // ("Creating Comm Object failed"), which gets worse with more devices.
        // Checked here UNDER enum_mutex, and ops drain enum_mutex after bumping
        // active_ops (see BusyGuard), so enumeration and a device open can never
        // overlap.
        if state.active_ops.load(Ordering::SeqCst) > 0 {
            return;
        }
        // In-process, fresh-context enumeration (see usb::list_devices).
        let listed = usb::list_devices();
        // A device must be absent for this many consecutive polls before it's
        // removed. Absorbs the transient drop-off when a device resets itself
        // (disconnect `rd 3`, or the maskrom→loader switch during `db`), which
        // otherwise makes it briefly disappear from the list.
        const REMOVE_AFTER: u32 = 3;
        {
            let mut map = state.devices.lock().unwrap();
            let present: std::collections::HashSet<u32> =
                listed.iter().map(|d| d.location).collect();

            // Add / update present devices.
            for d in &listed {
                let ds = map
                    .entry(d.location)
                    .or_insert_with(|| Arc::new(DeviceState::new(d.location)));
                let was_absent = ds.missed.swap(0, Ordering::SeqCst) > 0;
                ds.vid.store(d.vid, Ordering::SeqCst);
                ds.pid.store(d.pid, Ordering::SeqCst);
                *ds.mode.lock().unwrap() = d.mode.clone();
                // Re-appeared after being unplugged: it power-cycled, so any
                // loader that was in RAM is gone — drop stale connected/storage
                // state so the UI returns it to a fresh "Connect". Never do this
                // mid-operation (a device drops transiently during its own op).
                if was_absent && !ds.flash_running.load(Ordering::SeqCst) {
                    ds.reset_op_state();
                }
            }

            // Absent devices: count misses; remove only after the grace period
            // and only when idle (an in-flight op elsewhere shouldn't prune a
            // device that's mid-reset).
            // (We only reach here when active_ops == 0, so removing an absent
            // device can't prune one that's mid-operation.)
            map.retain(|loc, ds| {
                if present.contains(loc) {
                    return true;
                }
                let misses = ds.missed.fetch_add(1, Ordering::SeqCst) + 1;
                if misses >= REMOVE_AFTER {
                    if let Some(task) = ds.flash_task.lock().unwrap().as_ref() {
                        task.cancel();
                    }
                    false
                } else {
                    true
                }
            });
        }

        push_device_list_changed(app, state);
    }

    /// Push the device list to the UI only when its *rendered* state changed
    /// (identity + connection + storage), so repeated events/polls don't churn
    /// open dropdowns. Excludes live progress (patched via on_device_progress).
    fn push_device_list_changed(app: &AppHandle, state: &AppState) {
        let sig = {
            let map = state.devices.lock().unwrap();
            let mut parts: Vec<String> = map
                .values()
                .map(|d| {
                    format!(
                        "{}:{:04x}:{:04x}:{}:{}:{}",
                        d.location,
                        d.vid.load(Ordering::SeqCst),
                        d.pid.load(Ordering::SeqCst),
                        d.mode.lock().unwrap(),
                        d.loader_ready.load(Ordering::SeqCst),
                        d.available_storage_mask.load(Ordering::SeqCst),
                    )
                })
                .collect();
            parts.sort();
            parts.join("|")
        };
        let mut last = state.last_device_sig.lock().unwrap();
        if *last != sig {
            *last = sig.clone();
            drop(last);
            logging::write_line(&format!(
                "[app] devices: [{}]",
                if sig.is_empty() { "none".into() } else { sig }
            ));
            push_device_list(app, state);
        }
    }

    /// Apply a single hotplug event (macOS/Linux event-driven path). Builds the
    /// device map from arrival/removal events — no enumeration.
    #[cfg(not(windows))]
    fn apply_device_event(app: &AppHandle, state: &AppState, arrived: bool, dev: usb::UsbDevice) {
        {
            let mut map = state.devices.lock().unwrap();
            if arrived {
                // New device → fresh state. Existing entry (e.g. the maskrom→
                // loader re-enumeration during `db`) → just refresh identity/mode;
                // loader_ready is owned by the operation, not the event.
                let ds = map
                    .entry(dev.location)
                    .or_insert_with(|| Arc::new(DeviceState::new(dev.location)));
                ds.vid.store(dev.vid, Ordering::SeqCst);
                ds.pid.store(dev.pid, Ordering::SeqCst);
                *ds.mode.lock().unwrap() = dev.mode;
            } else if let Some(ds) = map.get(&dev.location) {
                if ds.flash_running.load(Ordering::SeqCst) {
                    // Device drops off transiently while it resets itself during
                    // its own op (db / rd) — keep it; the paired arrival follows.
                } else {
                    if let Some(task) = ds.flash_task.lock().unwrap().as_ref() {
                        task.cancel();
                    }
                    map.remove(&dev.location);
                }
            }
        }
        push_device_list_changed(app, state);
    }

    /// A command that should answer in milliseconds hit the probe timeout: the
    /// loader's USB state machine is wedged (seen on flaky host ports — an
    /// available storage answers `cs` fine, then the next command hangs).
    /// Nothing it reports can be trusted and a flash started now would sit at
    /// 0% forever, so clear the storage state and USB-reset the device back to
    /// a clean maskrom. Returns the user-facing error for the failed probe.
    fn probe_wedged(dev: &DeviceState) -> Result<(), String> {
        logging::write_line(&format!(
            "[app] storage probe timed out on 0x{:x} — loader wedged, USB-resetting",
            dev.location
        ));
        let msg = reset_wedged_device(dev.location);
        dev.available_storage_mask.store(0, Ordering::SeqCst);
        dev.selected_storage.store(0, Ordering::SeqCst);
        dev.last_storage_sectors.store(0, Ordering::SeqCst);
        Err(msg)
    }

    /// Probe eMMC/SD/SPI-NOR on one device, pick a default target, cache size.
    /// Errors if the loader stopped responding mid-probe (see probe_wedged) —
    /// the caller must fail its operation, not proceed.
    fn probe_storage_targets(dev: &DeviceState) -> Result<(), String> {
        let loc = Some(dev.location);
        let _guard = dev.probe_mutex.lock().unwrap();
        let mut mask = 0u32;
        for storage in [STORAGE_EMMC, STORAGE_SD] {
            let (res, _) = rkdev::run_sync_output(loc, &["cs", &storage.to_string()]);
            if res.was_cancelled {
                return probe_wedged(dev);
            }
            if res.exit_code == 0 {
                mask |= storage_bit(storage);
            }
        }
        // Probing SPI NOR is dangerous: `cs` makes the loader actually attempt
        // the switch (the protocol accepts it even for absent storage — absence
        // is only detected by reading the selection back), and on at least
        // rk3588_spl_loader v1.21.114 a failed SPI NOR init poisons the loader:
        // every later storage command hangs until the board is power-cycled.
        // Only probe it when neither eMMC nor SD answered — then there is
        // nothing to lose, and no storage command follows a failed attempt.
        if mask == 0 {
            let (res, _) = rkdev::run_sync_output(loc, &["cs", &STORAGE_SPI_NOR.to_string()]);
            if res.was_cancelled {
                return probe_wedged(dev);
            }
            if res.exit_code == 0 {
                mask |= storage_bit(STORAGE_SPI_NOR);
            }
        }
        dev.available_storage_mask.store(mask, Ordering::SeqCst);
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
            let (res, _) = rkdev::run_sync_output(loc, &["cs", &selected.to_string()]);
            if res.was_cancelled {
                return probe_wedged(dev);
            }
            dev.selected_storage.store(selected, Ordering::SeqCst);
            // Capture the raw flash info once (shown as the device's tooltip).
            let (rfi_res, rfi) = rkdev::run_sync_output(loc, &["rfi"]);
            if rfi_res.was_cancelled {
                return probe_wedged(dev);
            }
            *dev.flash_info.lock().unwrap() = rfi.trim().to_string();
            // SD capacity from rfi is unreliable — never cache/display it.
            if selected == STORAGE_SD {
                dev.last_storage_sectors.store(0, Ordering::SeqCst);
            } else {
                let sectors = parse_flash_size_sectors(&rfi);
                dev.last_storage_sectors.store(sectors, Ordering::SeqCst);
            }
        }
        dev.storage_probe_complete.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Kill a streaming op after this long with zero output. A healthy wl/rl/ef
    /// prints a progress line every few seconds even on slow storage; total
    /// silence means the USB transfer stalled (wedged loader or flaky host
    /// port) and would otherwise sit at 0% forever.
    const FLASH_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    fn format_size(bytes: u64) -> String {
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        const MIB: f64 = 1024.0 * 1024.0;
        let b = bytes as f64;
        if b >= GIB {
            format!("{:.1} GiB", b / GIB)
        } else {
            format!("{:.0} MiB", b / MIB)
        }
    }

    fn format_duration(secs: u64) -> String {
        let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
        if h > 0 {
            format!("{h}h {m}m {s}s")
        } else if m > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{s}s")
        }
    }

    /// Completion summary for a successful streaming op: "7.4 GiB in 3m 12s
    /// (39.5 MB/s)", or just "in 3m 12s" when the byte total isn't known
    /// (quick erase). Speed is decimal MB/s, the convention for transfer rates.
    fn format_op_stats(total_bytes: Option<u64>, elapsed: std::time::Duration) -> String {
        let secs = elapsed.as_secs_f64();
        let dur = format_duration(elapsed.as_secs().max(1));
        match total_bytes {
            Some(bytes) if bytes > 0 && secs > 0.0 => format!(
                "{} in {} ({:.1} MB/s)",
                format_size(bytes),
                dur,
                bytes as f64 / secs / 1_000_000.0
            ),
            _ => format!("in {dur}"),
        }
    }

    /// Spawn a device-scoped rkdeveloptool operation. Progress and completion are
    /// reported per-device, so many of these can run concurrently on different
    /// boards. `op` labels the operation for the UI ("connect", "flash", ...).
    #[allow(clippy::too_many_arguments)]
    fn start_flash_task(
        app: AppHandle,
        state: Arc<AppState>,
        dev: Arc<DeviceState>,
        op: &str,
        args: Vec<String>,
        // Bytes this op will transfer (image size / storage capacity), for the
        // completion stats line. None when unknown (quick erase → time only).
        total_bytes: Option<u64>,
        cleanup: Option<Box<dyn FnOnce() + Send>>,
    ) -> bool {
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        dev.cancel_requested.store(false, Ordering::SeqCst);
        state.active_ops.fetch_add(1, Ordering::SeqCst);
        // Drain any in-flight enumeration so this device open doesn't collide
        // with the poll's get_device_list (see BusyGuard / emit_device_list).
        drop(state.enum_mutex.lock().unwrap());
        *dev.current_op.lock().unwrap() = op.to_string();
        dev.progress.store(0, Ordering::SeqCst);

        let location = dev.location;
        on_device_progress(&app, location, 0);
        push_device_list(&app, &state);

        // Stall watchdog state: on_line refreshes last_activity, the watchdog
        // thread trips `stalled` and kills the task when it goes quiet, and
        // on_exit sets `done` (and reads `stalled` to reword the failure).
        let last_activity = Arc::new(Mutex::new(std::time::Instant::now()));
        let stalled = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));

        let last_percent = Arc::new(Mutex::new(-1i32));
        let app_line = app.clone();
        let dev_line = dev.clone();
        let activity_line = last_activity.clone();
        let on_line = move |line: String| {
            *activity_line.lock().unwrap() = std::time::Instant::now();
            if let Some(p) = rkdev::parse_progress_percent(&line) {
                let mut last = last_percent.lock().unwrap();
                if p != *last {
                    *last = p;
                    dev_line.progress.store(p, Ordering::SeqCst);
                    on_device_progress(&app_line, location, p);
                }
            }
        };

        let app_exit = app.clone();
        let state_exit = state.clone();
        let dev_exit = dev.clone();
        let op_owned = op.to_string();
        let cleanup = Mutex::new(cleanup);
        let stalled_exit = stalled.clone();
        let done_exit = done.clone();
        let op_started = std::time::Instant::now();

        let task = match rkdev::start(
            Some(location),
            args,
            on_line,
            move |result: ProcessResult| {
                done_exit.store(true, Ordering::SeqCst);
                *dev_exit.flash_task.lock().unwrap() = None;
                if let Ok(mut c) = cleanup.lock() {
                    if let Some(f) = c.take() {
                        f();
                    }
                }

                // A stall-kill is a failure, not a user cancel — and the USB
                // reset that follows it drops the loader, so clear the
                // connected/storage state (the device returns as fresh maskrom).
                let was_stalled = stalled_exit.load(Ordering::SeqCst);
                if was_stalled {
                    dev_exit.reset_op_state();
                }
                let cancelled = result.was_cancelled && !was_stalled;
                let success = result.exit_code == 0
                    && result.error_message.is_empty()
                    && !result.was_cancelled;

                dev_exit.flash_running.store(false, Ordering::SeqCst);
                *dev_exit.current_op.lock().unwrap() = String::new();
                dev_exit
                    .progress
                    .store(if success { 100 } else { -1 }, Ordering::SeqCst);
                state_exit.active_ops.fetch_sub(1, Ordering::SeqCst);

                let err = if was_stalled {
                    format!(
                        "no progress for {}s — the USB transfer stalled, so the operation \
                         was aborted and a USB reset attempted. Connect and try again; if \
                         the device doesn't reappear, unplug and replug it (a different \
                         USB port may help).",
                        FLASH_STALL_TIMEOUT.as_secs()
                    )
                } else if !success && !cancelled {
                    if result.error_message.is_empty() {
                        format!("rkdeveloptool failed with exit code {}", result.exit_code)
                    } else {
                        result.error_message
                    }
                } else {
                    String::new()
                };
                let stats = if success {
                    let s = format_op_stats(total_bytes, op_started.elapsed());
                    logging::write_line(&format!("[app] {op_owned} done (0x{location:x}): {s}"));
                    s
                } else {
                    String::new()
                };
                on_device_op_complete(
                    &app_exit, location, &op_owned, success, cancelled, &err, &stats,
                );
                push_device_list(&app_exit, &state_exit);
            },
        ) {
            Ok(t) => t,
            Err(e) => {
                dev.flash_running.store(false, Ordering::SeqCst);
                *dev.current_op.lock().unwrap() = String::new();
                dev.progress.store(-1, Ordering::SeqCst);
                state.active_ops.fetch_sub(1, Ordering::SeqCst);
                on_device_op_complete(&app, location, op, false, false, &e, "");
                return false;
            }
        };

        *dev.flash_task.lock().unwrap() = Some(task.clone());

        // Watchdog: kill the op if it goes silent, then USB-reset the device so
        // it doesn't stay wedged mid-transfer (that state survives the kill and
        // makes every later transfer hang too). `stalled` is set before the
        // kill so on_exit reports a stall instead of a cancel.
        let op_watch = op.to_string();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if done.load(Ordering::SeqCst) {
                    return;
                }
                if last_activity.lock().unwrap().elapsed() >= FLASH_STALL_TIMEOUT {
                    break;
                }
            }
            logging::write_line(&format!(
                "[app] {op_watch} stalled on 0x{location:x} — no output for {}s; killing and USB-resetting",
                FLASH_STALL_TIMEOUT.as_secs()
            ));
            stalled.store(true, Ordering::SeqCst);
            task.cancel();
            // Let the killed child release its USB handle before we open+reset.
            std::thread::sleep(std::time::Duration::from_millis(500));
            match usb::reset_device(location) {
                Ok(()) => logging::write_line("[app] USB reset ok — device back in maskrom"),
                Err(e) => logging::write_line(&format!("[app] USB reset failed: {e}")),
            }
        });
        true
    }

    /// Timeouts for quick, non-streaming ops. A device that doesn't respond
    /// within these is reported as an error instead of hanging forever. (Flash/
    /// erase/backup have no timeout — they can legitimately take minutes.)
    const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    const RESET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

    /// USB-reset a wedged device; log the outcome and return the user-facing
    /// error message for the operation that just failed (honest about whether
    /// the reset worked — on Windows it never does and the user must replug).
    fn reset_wedged_device(location: u32) -> String {
        match usb::reset_device(location) {
            Ok(()) => {
                logging::write_line("[app] USB reset ok — device back in maskrom");
                "device stopped responding and was USB-reset back to maskrom — try Connect again"
                    .to_string()
            }
            Err(e) => {
                logging::write_line(&format!("[app] USB reset failed: {e}"));
                "device stopped responding and could not be USB-reset — unplug and replug it, \
                 then try Connect again"
                    .to_string()
            }
        }
    }

    /// Run a quick non-streaming reset op (disconnect/reboot) on a worker thread.
    /// Uses run_sync, which retries transient "Creating Comm Object failed" open
    /// errors and kills on `timeout` — so these never hang the UI. On success the
    /// device leaves loader mode, so its op state is reset.
    fn spawn_reset_op(
        app: AppHandle,
        state: Arc<AppState>,
        dev: Arc<DeviceState>,
        op: &'static str,
        args: Vec<String>,
        timeout: std::time::Duration,
    ) -> bool {
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        dev.cancel_requested.store(false, Ordering::SeqCst);
        state.active_ops.fetch_add(1, Ordering::SeqCst);
        drop(state.enum_mutex.lock().unwrap());
        *dev.current_op.lock().unwrap() = op.to_string();
        dev.progress.store(0, Ordering::SeqCst);
        let location = dev.location;
        on_device_progress(&app, location, 0);
        push_device_list(&app, &state);

        std::thread::spawn(move || {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let res = rkdev::run_sync(Some(location), &argv, Some(timeout));
            let success = res.exit_code == 0 && !res.was_cancelled;
            if success {
                dev.reset_op_state();
            }
            dev.flash_running.store(false, Ordering::SeqCst);
            *dev.current_op.lock().unwrap() = String::new();
            dev.progress
                .store(if success { 100 } else { -1 }, Ordering::SeqCst);
            state.active_ops.fetch_sub(1, Ordering::SeqCst);
            let err = if success {
                String::new()
            } else if res.was_cancelled {
                format!("{op} timed out — device not responding")
            } else if res.error_message.is_empty() {
                "operation failed".to_string()
            } else {
                res.error_message
            };
            on_device_op_complete(&app, location, op, success, false, &err, "");
            push_device_list(&app, &state);
        });
        true
    }

    /// Kill any orphaned rkdeveloptool processes left behind by a previous
    /// session that crashed or was force-quit mid-operation. A hung child (e.g. a
    /// `db` stalled in a USB transfer) keeps the target device's USB handle open,
    /// so every later attempt to open *that* device — by this app or anything
    /// else — fails with "Creating Comm Object failed" until the process dies.
    /// The per-command timeout prevents new hangs, but a child orphaned at quit is
    /// reparented to the OS and can outlive us, so we sweep at startup before we
    /// touch any device. Best-effort: ignore errors (nothing to kill is normal).
    pub fn kill_stray_rkdeveloptool() {
        #[cfg(not(windows))]
        let result = std::process::Command::new("pkill")
            .args(["-9", "-f", "rkdeveloptool"])
            .status();
        #[cfg(windows)]
        let result = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/IM", "rkdeveloptool.exe"])
            .status();
        // exit 0 = killed something, 1 = nothing matched; both are fine.
        if matches!(result, Ok(s) if s.success()) {
            logging::write_line("[app] cleaned up an orphaned rkdeveloptool process from a prior session");
        }
    }

    /// Logged once at launch: this app's version and rkdeveloptool's version.
    /// Spawns rkdeveloptool directly (not via the logged runner) so the version
    /// check doesn't add `[rkdev]` noise to the top of the log.
    pub fn log_startup_versions(app_version: &str) {
        logging::write_line(&format!("[app] app version {app_version}"));
        let ver = match paths::rkdeveloptool_path() {
            Ok(path) => std::process::Command::new(path)
                .arg("-v")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.lines().next().unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "rkdeveloptool version unavailable".to_string()),
            Err(e) => format!("rkdeveloptool not found ({e})"),
        };
        logging::write_line(&format!("[app] {ver}"));
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
        // macOS/Linux: fully event-driven. libusb hotplug delivers arrival/
        // removal callbacks (including present devices at startup, via
        // enumerate=true); no polling, no bus enumeration.
        #[cfg(not(windows))]
        {
            let app_c = app.clone();
            let state_c = state.inner().clone();
            let cb = std::sync::Arc::new(move |arrived: bool, dev: usb::UsbDevice| {
                apply_device_event(&app_c, &state_c, arrived, dev);
            });
            let _ = usb::start(cb);
        }

        // Windows: native device notifications wake a poll that enumerates via
        // `rkdeveloptool ld` (no libusb hotplug on Windows).
        #[cfg(windows)]
        {
            let app_c = app.clone();
            let state_c = state.inner().clone();
            let on_usb = std::sync::Arc::new(move |_present: bool, _vid: u16, _pid: u16| {
                emit_device_list(&app_c, &state_c);
            });
            let _ = usb::start(on_usb);
            emit_device_list(&app, state.inner());
            let app_poll = app.clone();
            let state_poll = state.inner().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                emit_device_list(&app_poll, &state_poll);
            });
        }
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
    pub fn flash_bootloader(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult {
                started: false,
                error: "device is no longer present".into(),
            };
        };
        // Claim this device for the whole Connect path (td probe + optional db).
        // Every rkdeveloptool call runs on a worker thread with its own timeout
        // (td 5s, db CONNECT_TIMEOUT), never the invoke thread, so a flaky device
        // errors out instead of hanging. Other devices are unaffected (own claims).
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        dev.cancel_requested.store(false, Ordering::SeqCst);
        state.active_ops.fetch_add(1, Ordering::SeqCst);
        // Drain any in-flight enumeration before the td/db opens (see BusyGuard).
        drop(state.enum_mutex.lock().unwrap());
        *dev.current_op.lock().unwrap() = "connect".into();
        dev.progress.store(0, Ordering::SeqCst);
        let app = app.clone();
        let state = state.inner().clone();
        push_device_list(&app, &state);

        std::thread::spawn(move || {
            let loc = Some(location);
            let (td_timed_out, already_ready) = {
                let _g = dev.probe_mutex.lock().unwrap();
                let (res, _) = rkdev::run_sync_output(loc, &["td"]);
                (res.was_cancelled, res.exit_code == 0)
            };

            // Release the claim + emit completion for every Connect exit path.
            let finish = |success: bool, cancelled: bool, err: &str, keep_loader: bool| {
                if keep_loader {
                    dev.loader_ready.store(true, Ordering::SeqCst);
                }
                dev.flash_running.store(false, Ordering::SeqCst);
                *dev.current_op.lock().unwrap() = String::new();
                dev.progress
                    .store(if success { 100 } else { -1 }, Ordering::SeqCst);
                state.active_ops.fetch_sub(1, Ordering::SeqCst);
                on_device_op_complete(&app, location, "connect", success, cancelled, err, "");
                push_device_list(&app, &state);
            };
            let cancel_pending = || dev.cancel_requested.swap(false, Ordering::SeqCst);

            if cancel_pending() {
                finish(false, true, "", false);
                return;
            }

            // In both maskrom and loader mode `td` answers within milliseconds
            // (even "Test Device failed!" is a prompt reply). Hitting the probe
            // timeout means a wedged loader from an earlier session — a `db`
            // against it would just burn CONNECT_TIMEOUT and stall too. Reset
            // it back to maskrom now and have the user reconnect.
            if td_timed_out {
                logging::write_line(&format!(
                    "[app] Connect: td timed out on 0x{location:x} — device wedged, USB-resetting"
                ));
                finish(false, false, &reset_wedged_device(location), false);
                return;
            }

            if already_ready {
                logging::write_line("[app] Connect: loader already running");
                {
                    let _g = dev.probe_mutex.lock().unwrap();
                    let (_, chip) = rkdev::run_sync_output(loc, &["rci"]);
                    for line in chip.lines() {
                        if line.to_lowercase().contains("chip") {
                            logging::write_line(&format!("[app] {line}"));
                        }
                    }
                }
                if cancel_pending() {
                    finish(false, true, "", false);
                    return;
                }
                if let Err(e) = probe_storage_targets(&dev) {
                    finish(false, false, &e, false);
                    return;
                }
                if cancel_pending() {
                    finish(false, true, "", true);
                    return;
                }
                finish(true, false, "", true);
                return;
            }

            // Maskrom: need SPL download. Resolve the loader, then run `db` here
            // (synchronously, on this worker) with CONNECT_TIMEOUT.
            if cancel_pending() {
                finish(false, true, "", false);
                return;
            }
            let vid = dev.vid.load(Ordering::SeqCst);
            let pid = dev.pid.load(Ordering::SeqCst);
            let loader = match loader_map::entry_for(vid, pid) {
                None => {
                    finish(false, false, &format!(
                        "unrecognized device (VID 0x{vid:04X} PID 0x{pid:04X}) - not a supported Rockchip SoC"
                    ), false);
                    return;
                }
                Some(entry) => match entry.filename {
                    None => {
                        finish(false, false, &format!(
                            "{} is not supported - no loader is available for this SoC",
                            entry.soc
                        ), false);
                        return;
                    }
                    Some(filename) => match paths::loader_path(filename) {
                        None => {
                            finish(false, false, &format!("loader file not found: {filename}"), false);
                            return;
                        }
                        Some(p) => p,
                    },
                },
            };

            // Read the loader from the app itself (a GUI process) before handing
            // it to the rkdeveloptool child. If the loader lives in a TCC-guarded
            // folder (~/Desktop, ~/Documents, ~/Downloads), this is what makes
            // macOS show the file-access prompt: a spawned CLI child can't prompt,
            // so without this its read is silently denied ("Opening loader
            // failed"). Once the user grants access here, the child inherits it
            // (the app is the responsible process).
            if let Err(e) = std::fs::read(&loader) {
                finish(
                    false,
                    false,
                    &format!(
                        "cannot read loader {} ({}). If it's in Desktop/Documents/Downloads, \
                         allow file access when macOS asks, then try Connect again.",
                        loader.display(),
                        e
                    ),
                    false,
                );
                return;
            }

            // Download the SPL loader synchronously. run_sync retries transient
            // "Creating Comm Object failed" opens and kills on CONNECT_TIMEOUT, so
            // Connect never hangs. (No streaming needed — Connect shows dots.)
            logging::write_line(&format!("[app] Connect: download boot {}", loader.display()));
            let loader_str = loader.to_string_lossy().into_owned();
            let db_res = rkdev::run_sync(loc, &["db", loader_str.as_str()], Some(CONNECT_TIMEOUT));
            if db_res.was_cancelled {
                // A db timeout is not proof of failure: the loader takes over
                // the USB port mid-command (maskrom→loader re-enumeration), and
                // if the success reply is lost in that hand-off the transfer
                // completed but rkdeveloptool hangs waiting for it. The same is
                // true when the loader was already running (e.g. the app
                // restarted after a sudden reboot) — db into it hangs. `td`
                // distinguishes: the BootROM answers it with a prompt failure,
                // a live loader with OK. Never USB-reset here: a reset cannot
                // revive a dead device, but it does drop a live loader.
                logging::write_line(&format!(
                    "[app] Connect: db timed out on 0x{location:x} — probing whether the loader is up anyway"
                ));
                // Let the maskrom→loader re-enumeration settle before probing.
                std::thread::sleep(std::time::Duration::from_secs(2));
                let loader_up = {
                    let _g = dev.probe_mutex.lock().unwrap();
                    let (res, _) = rkdev::run_sync_output(loc, &["td"]);
                    res.exit_code == 0
                };
                if !loader_up {
                    finish(
                        false,
                        false,
                        "Connect timed out — device not responding; power-cycle or replug the \
                         board, then try Connect again",
                        false,
                    );
                    return;
                }
                logging::write_line(
                    "[app] Connect: loader is running — continuing despite db timeout",
                );
            } else if db_res.exit_code != 0 {
                let err = if rkdev::is_open_failure(&db_res.error_message) {
                    // The device enumerated (we can see it) but libusb_open kept
                    // failing across retries — almost always a cable/port/board
                    // fault rather than the app. See is_open_failure().
                    "USB open failed after retries (Creating Comm Object failed) — the device \
                     is visible but can't be opened. Try a different USB cable and port. If it \
                     only ever fails on this one board, that board's USB is the likely cause."
                        .to_string()
                } else if db_res.error_message.is_empty() {
                    "download boot failed".to_string()
                } else {
                    db_res.error_message
                };
                finish(false, false, &err, false);
                return;
            }
            if cancel_pending() {
                finish(false, true, "", false);
                return;
            }

            // Loader is running: read chip info, probe storage, mark connected.
            {
                let _g = dev.probe_mutex.lock().unwrap();
                let (_, chip) = rkdev::run_sync_output(loc, &["rci"]);
                for line in chip.lines() {
                    if line.to_lowercase().contains("chip") {
                        logging::write_line(&format!("[app] {line}"));
                    }
                }
            }
            if let Err(e) = probe_storage_targets(&dev) {
                finish(false, false, &e, false);
                return;
            }
            finish(true, false, "", true);
        });

        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn disconnect_device(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        if !dev.loader_ready.load(Ordering::SeqCst) {
            return StartResult {
                started: false,
                error: "device is not connected".into(),
            };
        }
        logging::write_line("[app] Disconnect: resetting device to maskrom");
        // `rd 3` = RST_RESETMASKROM_SUBCODE: reset back into maskrom rather than
        // a plain `rd` (subcode 0), which would reboot into normal flash boot.
        if !spawn_reset_op(
            app,
            state.inner().clone(),
            dev,
            "disconnect",
            vec!["rd".into(), "3".into()],
            RESET_TIMEOUT,
        ) {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    /// Reboot the device into normal boot (runs whatever was just flashed).
    /// Plain `rd` = RST_NONE_SUBCODE (0): a normal reset, unlike Disconnect's
    /// `rd 3` which forces maskrom.
    #[tauri::command]
    pub fn reboot_device(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        if !dev.loader_ready.load(Ordering::SeqCst) {
            return StartResult {
                started: false,
                error: "device is not connected".into(),
            };
        }
        logging::write_line(&format!("[app] Reboot: resetting 0x{location:x} to normal boot"));
        if !spawn_reset_op(
            app,
            state.inner().clone(),
            dev,
            "reboot",
            vec!["rd".into()],
            RESET_TIMEOUT,
        ) {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
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
        location: u32,
        image_path: String,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
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
        logging::write_line(&format!("[app] Flash Image (0x{location:x}): {image_path}"));
        let image_bytes = fs::metadata(&path).map(|m| m.len()).ok();
        if !start_flash_task(
            app,
            state.inner().clone(),
            dev,
            "flash",
            vec!["wl".into(), "0".into(), image_path],
            image_bytes,
            None,
        ) {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn erase_storage(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        logging::write_line(&format!("[app] Quick Erase (0x{location:x})"));
        if !start_flash_task(
            app,
            state.inner().clone(),
            dev,
            "erase",
            vec!["ef".into()],
            None,
            None,
        ) {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn secure_erase_storage(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        let storage = dev.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return StartResult {
                started: false,
                error: "no storage target selected".into(),
            };
        }
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        let mut total_sectors = {
            let _busy = BusyGuard::new(state.inner());
            let _g = dev.probe_mutex.lock().unwrap();
            let (_, rfi) = rkdev::run_sync_output(Some(location), &["rfi"]);
            parse_flash_size_sectors(&rfi)
        };
        dev.flash_running.store(false, Ordering::SeqCst);
        let cached = dev.last_storage_sectors.load(Ordering::SeqCst);
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
            dev,
            "secure_erase",
            vec![
                "wl".into(),
                "0".into(),
                zero_path.to_string_lossy().into_owned(),
            ],
            Some(total_sectors * 512),
            Some(Box::new(move || {
                let _ = fs::remove_file(&zp);
            })),
        ) {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
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
        location: u32,
        dest_path: String,
        force: bool,
    ) -> BackupStartResult {
        let Some(dev) = get_device(&state, location) else {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "device is no longer present".into(),
            };
        };
        if dest_path.is_empty() {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "no destination selected".into(),
            };
        }
        let storage = dev.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "no storage target selected".into(),
            };
        }
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "operation already in progress".into(),
            };
        }

        let (mut main_sectors, mut total_sectors) = {
            let _busy = BusyGuard::new(state.inner());
            let _g = dev.probe_mutex.lock().unwrap();
            let main = rkdev::read_gpt_info(Some(location)).map(|g| g.last_used_lba + 1).unwrap_or(0);
            let (_, rfi) = rkdev::run_sync_output(Some(location), &["rfi"]);
            let total = parse_flash_size_sectors(&rfi);
            (main, total)
        };
        dev.flash_running.store(false, Ordering::SeqCst);
        let cached = dev.last_storage_sectors.load(Ordering::SeqCst);
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
            "[app] Backup {} (0x{location:x}): {main_sectors} sectors -> {dest_path}",
            storage_name(storage)
        ));
        if !start_flash_task(
            app,
            state.inner().clone(),
            dev,
            "backup",
            vec![
                "rl".into(),
                "0".into(),
                main_sectors.to_string(),
                dest_path,
            ],
            Some(main_sectors * 512),
            None,
        ) {
            return BackupStartResult {
                started: false,
                needs_confirmation: false,
                message: "operation already in progress".into(),
            };
        }
        BackupStartResult {
            started: true,
            needs_confirmation: false,
            message: String::new(),
        }
    }

    #[tauri::command]
    pub fn cancel_flash(
        app: AppHandle,
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        logging::write_line(&format!("[app] Cancel requested (0x{location:x})"));
        dev.cancel_requested.store(true, Ordering::SeqCst);

        let had_task = {
            let guard = dev.flash_task.lock().unwrap();
            if let Some(task) = guard.as_ref() {
                task.cancel();
                true
            } else {
                false
            }
        };

        if had_task {
            // on_exit of the rkdev task will emit on_device_op_complete(cancelled).
            return StartResult {
                started: true,
                error: String::new(),
            };
        }

        // No live rkdeveloptool process (connect probe-only path, or a stuck
        // claim). Unlock immediately so Cancel always ends the operation.
        if dev.flash_running.swap(false, Ordering::SeqCst) {
            logging::write_line("[app] Cancel: no rkdev task — unlocking device");
            *dev.current_op.lock().unwrap() = String::new();
            dev.progress.store(-1, Ordering::SeqCst);
            state.active_ops.fetch_sub(1, Ordering::SeqCst);
            on_device_op_complete(&app, location, "cancel", false, true, "", "");
            push_device_list(&app, state.inner());
            StartResult {
                started: true,
                error: String::new(),
            }
        } else {
            StartResult {
                started: false,
                error: "no operation in progress".into(),
            }
        }
    }

    #[tauri::command]
    pub fn force_close_window(app: AppHandle, state: State<'_, Arc<AppState>>) -> bool {
        for dev in state.devices.lock().unwrap().values() {
            if let Some(task) = dev.flash_task.lock().unwrap().as_ref() {
                task.cancel();
            }
        }
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.close();
        }
        true
    }

    /// On window close, reset every connected device back to maskrom (`rd 3`) so
    /// nothing is left stuck in loader mode. Returns true if the close should be
    /// deferred while this runs asynchronously; false if it can proceed now.
    pub fn begin_close_cleanup(app: &AppHandle, state: Arc<AppState>) -> bool {
        // Cleanup already finished (this is the programmatic re-close) → let it go.
        if state.close_ready.load(Ordering::SeqCst) {
            return false;
        }

        let connected: Vec<u32> = {
            let map = state.devices.lock().unwrap();
            map.values()
                .filter(|d| d.loader_ready.load(Ordering::SeqCst))
                .map(|d| d.location)
                .collect()
        };
        let anything_running = state.active_ops.load(Ordering::SeqCst) > 0;

        // Nothing in loader mode and nothing running → close immediately.
        if connected.is_empty() && !anything_running {
            return false;
        }
        // Cleanup already in flight → keep deferring the close.
        if state.cleanup_started.swap(true, Ordering::SeqCst) {
            return true;
        }

        logging::write_line(&format!(
            "[app] Quit: resetting {} connected device(s) to maskrom",
            connected.len()
        ));
        let app = app.clone();
        std::thread::spawn(move || {
            // Pause enumeration while we reset devices (see BusyGuard).
            let _busy = BusyGuard::new(&state);
            // Stop any in-flight operations so `rd 3` doesn't collide with them.
            for dev in state.devices.lock().unwrap().values() {
                if let Some(task) = dev.flash_task.lock().unwrap().as_ref() {
                    task.cancel();
                }
            }
            // Reset each device that had a loader running back to maskrom.
            for loc in connected {
                let res = rkdev::run_sync(
                    Some(loc),
                    &["rd", "3"],
                    Some(std::time::Duration::from_secs(3)),
                );
                logging::write_line(&format!(
                    "[app] Quit: rd 3 on 0x{loc:x} -> {}",
                    if res.exit_code == 0 { "ok" } else { "failed" }
                ));
            }
            state.close_ready.store(true, Ordering::SeqCst);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.close();
            }
        });
        true
    }

    #[tauri::command]
    pub fn get_storage_info(
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StorageInfoResult {
        let Some(dev) = get_device(&state, location) else {
            return StorageInfoResult { success: false, storage_bytes: 0, error: "device is no longer present".into() };
        };
        let storage = dev.selected_storage.load(Ordering::SeqCst);
        if storage == 0 || !dev.loader_ready.load(Ordering::SeqCst) {
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
        let mut sectors = dev.last_storage_sectors.load(Ordering::SeqCst);
        if sectors == 0 {
            let _busy = BusyGuard::new(state.inner());
            let _g = dev.probe_mutex.lock().unwrap();
            // Ensure we're reading the currently selected target.
            let (cs, _) = rkdev::run_sync_output(Some(location), &["cs", &storage.to_string()]);
            if cs.exit_code != 0 {
                return StorageInfoResult {
                    success: false,
                    storage_bytes: 0,
                    error: format!("could not select {}", storage_name(storage)),
                };
            }
            let (_, rfi) = rkdev::run_sync_output(Some(location), &["rfi"]);
            sectors = parse_flash_size_sectors(&rfi);
            dev.last_storage_sectors.store(sectors, Ordering::SeqCst);
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
    pub fn get_storage_targets(
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> StorageTargetsResult {
        let Some(dev) = get_device(&state, location) else {
            return StorageTargetsResult {
                success: false,
                emmc_available: false,
                sd_available: false,
                spinor_available: false,
                selected_storage: 0,
                error: "device is no longer present".into(),
            };
        };
        let mask = dev.available_storage_mask.load(Ordering::SeqCst);
        StorageTargetsResult {
            success: dev.loader_ready.load(Ordering::SeqCst),
            emmc_available: mask & storage_bit(STORAGE_EMMC) != 0,
            sd_available: mask & storage_bit(STORAGE_SD) != 0,
            spinor_available: mask & storage_bit(STORAGE_SPI_NOR) != 0,
            selected_storage: dev.selected_storage.load(Ordering::SeqCst),
            error: String::new(),
        }
    }

    #[tauri::command]
    pub fn select_storage(
        state: State<'_, Arc<AppState>>,
        location: u32,
        storage: u32,
    ) -> StartResult {
        let Some(dev) = get_device(&state, location) else {
            return StartResult { started: false, error: "device is no longer present".into() };
        };
        if !dev.loader_ready.load(Ordering::SeqCst) {
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
        let mask = dev.available_storage_mask.load(Ordering::SeqCst);
        if mask & storage_bit(storage) == 0 {
            return StartResult {
                started: false,
                error: format!("{} not detected", storage_name(storage)),
            };
        }
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return StartResult {
                started: false,
                error: "operation already in progress".into(),
            };
        }
        let _busy = BusyGuard::new(state.inner());
        let _g = dev.probe_mutex.lock().unwrap();
        let (res, _) = rkdev::run_sync_output(Some(location), &["cs", &storage.to_string()]);
        dev.flash_running.store(false, Ordering::SeqCst);
        if res.exit_code != 0 {
            return StartResult {
                started: false,
                error: format!("{} not detected", storage_name(storage)),
            };
        }
        dev.selected_storage.store(storage, Ordering::SeqCst);
        logging::write_line(&format!(
            "[app] Storage selected (0x{location:x}): {}",
            storage_name(storage)
        ));
        // Never cache an SD size (rfi is unreliable there). Always refresh
        // capacity when switching to eMMC / SPI NOR.
        if storage == STORAGE_SD {
            dev.last_storage_sectors.store(0, Ordering::SeqCst);
        } else {
            let (_, rfi) = rkdev::run_sync_output(Some(location), &["rfi"]);
            let sectors = parse_flash_size_sectors(&rfi);
            dev.last_storage_sectors.store(sectors, Ordering::SeqCst);
        }
        StartResult {
            started: true,
            error: String::new(),
        }
    }

    /// Return the current device list for the UI. On macOS/Linux the map is kept
    /// live by hotplug events, so this just snapshots it. On Windows it triggers
    /// a fresh `ld` enumeration first.
    #[tauri::command]
    pub fn list_devices(app: AppHandle, state: State<'_, Arc<AppState>>) -> Vec<DeviceEntry> {
        #[cfg(windows)]
        emit_device_list(&app, state.inner());
        #[cfg(not(windows))]
        let _ = &app;
        let mut entries: Vec<DeviceEntry> = {
            let map = state.devices.lock().unwrap();
            map.values().map(|d| d.to_entry()).collect()
        };
        entries.sort_by_key(|e| e.location);
        entries
    }

    #[tauri::command]
    pub fn calculate_used_space(
        state: State<'_, Arc<AppState>>,
        location: u32,
    ) -> UsedSpaceResult {
        let Some(dev) = get_device(&state, location) else {
            return UsedSpaceResult { success: false, used_bytes: 0, error: "device is no longer present".into() };
        };
        if !dev.loader_ready.load(Ordering::SeqCst) {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "device is not connected".into(),
            };
        }
        let storage = dev.selected_storage.load(Ordering::SeqCst);
        if storage == 0 {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "no storage target selected".into(),
            };
        }
        if dev
            .flash_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return UsedSpaceResult {
                success: false,
                used_bytes: 0,
                error: "operation already in progress".into(),
            };
        }

        let _busy = BusyGuard::new(state.inner());
        let loc = Some(location);
        let label = storage_name(storage);
        logging::write_line(&format!("[app] Calculate Used Space (0x{location:x}): {label}"));

        let result = (|| {
            let _g = dev.probe_mutex.lock().unwrap();
            // Re-select the target on this device before rl probes.
            let (cs, _) = rkdev::run_sync_output(loc, &["cs", &storage.to_string()]);
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
                if let Some(gpt) = rkdev::read_gpt_info(loc) {
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
                let (_, rfi) = rkdev::run_sync_output(loc, &["rfi"]);
                let total = parse_flash_size_sectors(&rfi);
                if total == 0 {
                    return UsedSpaceResult {
                        success: false,
                        used_bytes: 0,
                        error: format!("could not determine used space on {label}"),
                    };
                }
                let used = rkdev::find_used_sector_boundary(loc, total);
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

            let mut total = dev.last_storage_sectors.load(Ordering::SeqCst);
            if total == 0 {
                let (_, rfi) = rkdev::run_sync_output(loc, &["rfi"]);
                total = parse_flash_size_sectors(&rfi);
                if total != 0 {
                    dev.last_storage_sectors.store(total, Ordering::SeqCst);
                }
            }
            if total == 0 {
                return UsedSpaceResult {
                    success: false,
                    used_bytes: 0,
                    error: format!("could not read {label} size"),
                };
            }
            let used = rkdev::find_used_sector_boundary(loc, total);
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

        dev.flash_running.store(false, Ordering::SeqCst);
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
            app::reboot_device,
            app::flash_image,
            app::erase_storage,
            app::secure_erase_storage,
            app::backup_storage,
            app::cancel_flash,
            app::force_close_window,
            app::get_storage_info,
            app::get_storage_targets,
            app::select_storage,
            app::list_devices,
            app::calculate_used_space,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            logging::set_ui_sink(move |line, replace| {
                app::append_live_log(&handle, &line, replace);
            });
            // Sweep orphaned rkdeveloptool children from a prior crashed/force-
            // quit session before we enumerate — a hung one holds a device's USB
            // handle and causes "Creating Comm Object failed" on every open.
            app::kill_stray_rkdeveloptool();
            app::log_startup_versions(&app.package_info().version.to_string());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Before quitting, reset any connected devices back to maskrom so
            // nothing is left stuck in loader mode. Defer the close until that
            // async cleanup finishes, then let the re-close through.
            tauri::WindowEvent::CloseRequested { api, .. } => {
                let app = window.app_handle().clone();
                let state = app.state::<std::sync::Arc<app::AppState>>().inner().clone();
                if app::begin_close_cleanup(&app, state) {
                    api.prevent_close();
                }
            }
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
