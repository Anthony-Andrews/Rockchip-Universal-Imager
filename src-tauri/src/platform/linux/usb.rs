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

/// Every directory udevd reads rules from (covers normal distros and NixOS,
/// where /etc/udev/rules.d is a symlink into the composed store path).
const UDEV_RULE_DIRS: &[&str] = &[
    "/etc/udev/rules.d",
    "/run/udev/rules.d",
    "/usr/lib/udev/rules.d",
    "/lib/udev/rules.d",
];

/// True if any active (non-comment) udev rule matches the Rockchip vendor ID,
/// regardless of which package or admin installed it or what it is named.
fn rockchip_rule_present() -> bool {
    for dir in UDEV_RULE_DIRS {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rules") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let found = content.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with('#') && line.contains("idVendor") && line.contains("2207")
            });
            if found {
                return true;
            }
        }
    }
    false
}

pub fn query() -> Status {
    let installed = rockchip_rule_present();
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
    // NixOS: /etc/udev/rules.d is a read-only symlink into the store — writing
    // there fails even as root. Point at the declarative options instead.
    if std::path::Path::new("/etc/NIXOS").exists() {
        return InstallResult {
            success: false,
            error_message: "NixOS detected — enable the flake module (programs.rockchip-universal-imager.enable = true) or add the rule via services.udev.extraRules, then rebuild".into(),
        };
    }
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
