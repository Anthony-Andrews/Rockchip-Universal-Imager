//! macOS: libusb for Rockchip **detection**; no driver install for flashing.

use crate::platform::flashing::{Kind, Status};

pub use crate::platform::libusb_hotplug::{start, stop, UsbCallback, UsbDevice};

pub fn query() -> Status {
    Status {
        kind: Kind::None,
        device_relevant: true,
        ready: true,
        detail: String::new(),
        error: String::new(),
    }
}
