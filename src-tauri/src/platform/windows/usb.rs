//! Windows: **native** USB detection; **libwdi** installs libusb-win32 so
//! libusb can open the device afterward (rkdeveloptool).
//!
//! Detection must not assume libusb is already bound to the device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::platform::flashing::{InstallOptions, InstallResult, Kind, Status};

pub type UsbCallback = Arc<dyn Fn(bool, u16, u16) + Send + Sync + 'static>;

/// Bus-level reset of a wedged device (see the unix impl in libusb_hotplug).
/// Windows detection doesn't link libusb, so recovery there stays manual
/// (replug the device); callers treat Err as "couldn't reset" and say so.
pub fn reset_device(_location: u32) -> Result<(), String> {
    Err("USB reset is not supported on Windows — unplug and replug the device".into())
}

struct MonitorState {
    stop: AtomicBool,
    join: Mutex<Option<JoinHandle<()>>>,
}

static MONITOR: Mutex<Option<Arc<MonitorState>>> = Mutex::new(None);

pub fn start(on_change: UsbCallback) -> bool {
    stop();
    let state = Arc::new(MonitorState {
        stop: AtomicBool::new(false),
        join: Mutex::new(None),
    });

    let state_c = state.clone();
    let handle = thread::spawn(move || native_monitor_loop(state_c, on_change));
    *state.join.lock().unwrap() = Some(handle);
    *MONITOR.lock().unwrap() = Some(state);
    crate::logging::write_line(
        "[app] Windows native USB monitoring started (implementation pending)",
    );
    true
}

pub fn stop() {
    let prev = MONITOR.lock().unwrap().take();
    if let Some(state) = prev {
        state.stop.store(true, Ordering::SeqCst);
        if let Some(j) = state.join.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

/// Placeholder until SetupAPI / WM_DEVICECHANGE path is ported.
fn native_monitor_loop(state: Arc<MonitorState>, _on_change: UsbCallback) {
    while !state.stop.load(Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(500));
    }
}

pub fn query() -> Status {
    // TODO: SetupAPI / libwdi — report current driver for Rockchip device.
    Status {
        kind: Kind::WindowsDriver,
        device_relevant: false,
        ready: false,
        detail: String::new(),
        error: "Windows libwdi driver install not yet ported".into(),
    }
}

pub fn install(_options: &InstallOptions) -> InstallResult {
    // TODO: elevated libwdi install of libusb-win32 for the detected device.
    InstallResult {
        success: false,
        error_message: "Windows libwdi driver install not yet ported".into(),
    }
}
