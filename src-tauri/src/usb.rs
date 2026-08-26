//! Cross-platform USB presence API for Rockchip devices (VID 0x2207).
//!
//! - **macOS / Linux:** libusb hotplug (`platform/*/usb` → `libusb_hotplug`).
//! - **Windows:** native device notifications (`platform/windows/usb`); does not
//!   assume a libusb driver is already installed.

// macOS/Linux are fully event-driven: `start(cb)` delivers `(arrived, UsbDevice)`
// hotplug events; there is no enumeration/polling function.
#[cfg(target_os = "linux")]
pub use crate::platform::linux::usb::{reset_device, start, stop, UsbDevice};
#[cfg(target_os = "macos")]
pub use crate::platform::macos::usb::{reset_device, start, stop, UsbDevice};

// UsbCallback is the closure type `start` takes; re-exported for completeness,
// not always named in-tree (callers often build the Arc inline).
#[allow(unused_imports)]
#[cfg(target_os = "linux")]
pub use crate::platform::linux::usb::UsbCallback;
#[allow(unused_imports)]
#[cfg(target_os = "macos")]
pub use crate::platform::macos::usb::UsbCallback;

// Windows uses native device notifications + a `ld`-based enumeration (no libusb
// hotplug); its callback is (present, vid, pid) and detection is poll-driven.
#[cfg(windows)]
pub use crate::platform::windows::usb::{reset_device, start, stop, UsbCallback};

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
compile_error!("USB monitoring is only implemented for windows, linux, and macos");

/// One attached Rockchip device (location = (bus << 8) | port, matching
/// rkdeveloptool's `-l` LocationID). Windows enumerates via `rkdeveloptool ld`.
#[cfg(windows)]
#[derive(Clone, Debug)]
pub struct UsbDevice {
    pub location: u32,
    pub vid: u16,
    pub pid: u16,
    pub mode: String,
}

#[cfg(windows)]
pub fn list_devices() -> Vec<UsbDevice> {
    crate::rkdev::list_devices()
        .into_iter()
        .map(|d| UsbDevice {
            location: d.location,
            vid: d.vid,
            pid: d.pid,
            mode: d.mode,
        })
        .collect()
}
