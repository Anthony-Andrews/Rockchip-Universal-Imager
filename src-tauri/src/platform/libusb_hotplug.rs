//! Rockchip USB device detection via **libusb hotplug** (macOS and Linux).
//!
//! Fully event-driven: a dedicated thread runs `libusb_handle_events`, which
//! blocks on IOKit/udev notifications and dispatches arrival/removal callbacks.
//! There is **no polling** and no `get_device_list` — the device list is built
//! entirely from the callbacks' own `Device` (its cached descriptor gives
//! vid/pid/bus/port/mode). That keeps USB bus chatter at zero and avoids the
//! enumeration-vs-open contention that the poll-based approach had.
//!
//! Caveat: this relies on libusb delivering every hotplug event. On macOS that
//! is usually but not always perfectly reliable; a missed event would leave a
//! device stuck until it's re-plugged. (A poll fallback is used only if the
//! platform reports no hotplug support at all.)
//!
//! Windows does NOT use this path — a Rockchip device may not have a
//! libusb-compatible driver until libwdi installs one; Windows detection lives
//! in `platform/windows/usb.rs` (native APIs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use rusb::{Context, Device, Hotplug, HotplugBuilder, Registration, UsbContext};

const ROCKCHIP_VID: u16 = 0x2207;

/// Callback: `(arrived, device)` — arrived=true for a plug, false for an unplug.
pub type UsbCallback = Arc<dyn Fn(bool, UsbDevice) + Send + Sync + 'static>;

/// One attached Rockchip device.
#[derive(Clone, Debug)]
pub struct UsbDevice {
    pub location: u32,
    pub vid: u16,
    pub pid: u16,
    pub mode: String, // "Maskrom" | "Loader"
}

/// Read a Rockchip device's identity from its (cached) descriptor. Returns None
/// for non-Rockchip devices or if the descriptor can't be read.
fn describe<T: UsbContext>(device: &Device<T>) -> Option<UsbDevice> {
    let desc = device.device_descriptor().ok()?;
    if desc.vendor_id() != ROCKCHIP_VID {
        return None;
    }
    let location = ((device.bus_number() as u32) << 8) | (device.port_number() as u32);
    // rkdeveloptool: maskrom if (bcdUSB & 1)==0, else loader.
    let mode = if desc.usb_version().sub_minor() & 1 == 1 {
        "Loader"
    } else {
        "Maskrom"
    };
    Some(UsbDevice {
        location,
        vid: desc.vendor_id(),
        pid: desc.product_id(),
        mode: mode.to_string(),
    })
}

struct MonitorState {
    stop: AtomicBool,
    join: Mutex<Option<JoinHandle<()>>>,
}

static MONITOR: Mutex<Option<Arc<MonitorState>>> = Mutex::new(None);

pub fn start(cb: UsbCallback) -> bool {
    stop();
    let state = Arc::new(MonitorState {
        stop: AtomicBool::new(false),
        join: Mutex::new(None),
    });
    let state_c = state.clone();
    let handle = thread::spawn(move || hotplug_loop(state_c, cb));
    *state.join.lock().unwrap() = Some(handle);
    *MONITOR.lock().unwrap() = Some(state);
    crate::logging::write_line("[app] libusb hotplug monitoring started");
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

fn hotplug_loop(state: Arc<MonitorState>, cb: UsbCallback) {
    struct Handler {
        cb: UsbCallback,
    }
    impl<T: UsbContext> Hotplug<T> for Handler {
        fn device_arrived(&mut self, device: Device<T>) {
            if let Some(d) = describe(&device) {
                crate::logging::write_line(&format!(
                    "[usb] arrived 0x{:x} ({:04x}:{:04x} {})",
                    d.location, d.vid, d.pid, d.mode
                ));
                (self.cb)(true, d);
            }
        }
        fn device_left(&mut self, device: Device<T>) {
            if let Some(d) = describe(&device) {
                crate::logging::write_line(&format!("[usb] left 0x{:x}", d.location));
                (self.cb)(false, d);
            }
        }
    }

    let Ok(ctx) = Context::new() else {
        crate::logging::write_line("[app] libusb init failed; USB detection unavailable");
        return;
    };

    if !rusb::has_hotplug() {
        crate::logging::write_line("[app] libusb hotplug unsupported; using poll fallback");
        poll_fallback(&state, &cb, &ctx);
        return;
    }

    // `enumerate(true)` fires device_arrived for devices already attached, so the
    // initial set is delivered the same way as later hotplugs.
    let reg: Registration<Context> = match HotplugBuilder::new()
        .enumerate(true)
        .vendor_id(ROCKCHIP_VID)
        .register(&ctx, Box::new(Handler { cb: cb.clone() }))
    {
        Ok(r) => r,
        Err(_) => {
            crate::logging::write_line("[app] hotplug register failed; using poll fallback");
            poll_fallback(&state, &cb, &ctx);
            return;
        }
    };

    // Blocks on notifications; the 1s timeout is only to re-check the stop flag.
    // No bus enumeration happens here.
    while !state.stop.load(Ordering::SeqCst) {
        let _ = ctx.handle_events(Some(std::time::Duration::from_secs(1)));
    }
    drop(reg);
}

/// Only used when the platform reports no hotplug support (not expected on
/// macOS/Linux). Diffs the device list and synthesizes arrival/removal events.
fn poll_fallback(state: &Arc<MonitorState>, cb: &UsbCallback, ctx: &Context) {
    use std::collections::HashMap;
    let mut last: HashMap<u32, UsbDevice> = HashMap::new();
    while !state.stop.load(Ordering::SeqCst) {
        let mut cur: HashMap<u32, UsbDevice> = HashMap::new();
        if let Ok(list) = ctx.devices() {
            for dev in list.iter() {
                if let Some(d) = describe(&dev) {
                    cur.insert(d.location, d);
                }
            }
        }
        for (loc, d) in &cur {
            if last.get(loc).map(|p| &p.mode) != Some(&d.mode) {
                cb(true, d.clone());
            }
        }
        for (loc, d) in &last {
            if !cur.contains_key(loc) {
                cb(false, d.clone());
            }
        }
        last = cur;
        thread::sleep(std::time::Duration::from_secs(2));
    }
}
