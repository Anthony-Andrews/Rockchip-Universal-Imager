//! Linux: libusb for Rockchip **detection**; udev for **flash access**.

use std::process::Command;

use crate::platform::flashing::{InstallOptions, InstallResult, Kind, Status};

pub use crate::platform::libusb_hotplug::{start, stop, UsbCallback, UsbDevice};

const RULES_PATH: &str = "/etc/udev/rules.d/99-rockchip-universal-imager-rockchip.rules";
const RULES_CONTENT: &str = "\
# Installed by Rockchip Universal Imager - allow non-root access to Rockchip\n\
# Maskrom/loader (RockUSB) devices.\n\
SUBSYSTEM==\"usb\", ATTR{idVendor}==\"2207\", MODE=\"0666\", TAG+=\"uaccess\"\n\
";

pub fn query() -> Status {
    let installed = std::path::Path::new(RULES_PATH).is_file();
    Status {
        kind: Kind::LinuxUdev,
        device_relevant: true,
        ready: installed,
        detail: if installed {
            "installed".into()
        } else {
            String::new()
        },
        error: if installed {
            String::new()
        } else {
            "udev rules: not installed — flashing may need root".into()
        },
    }
}

pub fn install(_options: &InstallOptions) -> InstallResult {
    crate::logging::write_line("[app] Installing udev rules via pkexec");
    let script = format!(
        "printf '%s' '{RULES_CONTENT}' > {RULES_PATH} && udevadm control --reload-rules && udevadm trigger"
    );
    let output = Command::new("pkexec")
        .args(["/bin/sh", "-c", &script])
        .output();
    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(1);
            if code == 126 || code == 127 {
                return InstallResult {
                    success: false,
                    error_message: "authorization was dismissed".into(),
                };
            }
            if code != 0 {
                let msg = String::from_utf8_lossy(&o.stderr);
                return InstallResult {
                    success: false,
                    error_message: if msg.trim().is_empty() {
                        format!("udev rules install failed (exit {code})")
                    } else {
                        msg.trim().to_string()
                    },
                };
            }
            crate::logging::write_line("[app] udev rules installed");
            InstallResult {
                success: true,
                error_message: String::new(),
            }
        }
        Err(_) => InstallResult {
            success: false,
            error_message: "failed to start pkexec (is polkit installed?)".into(),
        },
    }
}
