//! Resolve app / companion / resource directories (portable-first layout).
//!
//! **macOS:** release packaging keeps companions *beside* the `.app`
//! (`rkdeveloptool` + `loader_binaries/` next to `Rockchip Universal Imager.app`).
//! Paths also accept companions inside the bundle (`Contents/MacOS`,
//! `Contents/Resources`) for local/dev convenience.

use std::path::{Path, PathBuf};

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

pub fn executable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Best-effort path to the `.app` bundle root on macOS (`Foo.app`).
#[cfg(target_os = "macos")]
fn app_bundle_root() -> Option<PathBuf> {
    let exe_dir = executable_dir();
    // .../Foo.app/Contents/MacOS
    let contents = exe_dir.parent()?;
    if exe_dir.file_name().and_then(|s| s.to_str()) != Some("MacOS") {
        return None;
    }
    if contents.file_name().and_then(|s| s.to_str()) != Some("Contents") {
        return None;
    }
    let bundle = contents.parent()?;
    if bundle.extension().and_then(|s| s.to_str()) == Some("app") {
        Some(bundle.to_path_buf())
    } else {
        None
    }
}

/// Directory for companions (rkdeveloptool, portable marker, optional libusb).
///
/// On macOS this is the directory *containing* the `.app` (legacy portable /
/// install-folder layout). Bundled companions live inside the app and are found
/// via dedicated candidate paths, not only via this directory.
pub fn companion_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = app_bundle_root() {
            if let Some(parent) = bundle.parent() {
                return parent.to_path_buf();
            }
        }
    }
    executable_dir()
}

pub fn is_portable_build() -> bool {
    companion_dir().join("portable").is_file()
}

pub fn rkdeveloptool_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "rkdeveloptool.exe"
    } else {
        "rkdeveloptool"
    }
}

fn first_existing_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    for path in candidates {
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

pub fn rkdeveloptool_path() -> Result<PathBuf, String> {
    let name = rkdeveloptool_name();
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1) Same directory as the GUI binary (macOS: Contents/MacOS when embedded)
    candidates.push(executable_dir().join(name));

    // 2) Next to the .app / portable tree root
    candidates.push(companion_dir().join(name));

    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = app_bundle_root() {
            let contents = bundle.join("Contents");
            // Embedded packaging locations
            candidates.push(contents.join("MacOS").join(name));
            candidates.push(contents.join("Helpers").join(name));
            candidates.push(contents.join("Resources").join(name));
            // Legacy: companions were placed beside the .app
            if let Some(parent) = bundle.parent() {
                candidates.push(parent.join(name));
            }
        }
    }

    // Dev convenience: repo-adjacent build
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target")
            .join(name),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/release")
            .join(name),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/debug")
            .join(name),
    );

    if let Some(path) = first_existing_file(candidates) {
        return Ok(path);
    }

    Err(format!(
        "rkdeveloptool is missing — expected `{}` inside the app (Contents/MacOS) or next to it.",
        name
    ))
}

pub fn resource_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Some(resources) = executable_dir().parent().map(|p| p.join("Resources")) {
            if resources.is_dir() {
                return resources;
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(bundle) = app_bundle_root() {
            let resources = bundle.join("Contents/Resources");
            if resources.is_dir() {
                return resources;
            }
        }
    }
    executable_dir()
}

/// loader_binaries next to app, in resources, or from repo during dev.
pub fn loader_binaries_dir() -> PathBuf {
    // `mut` is only used on macOS (the cfg block below inserts entries).
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut candidates = vec![
        // Bundled inside .app (preferred on macOS)
        resource_dir().join("loader_binaries"),
        // Next to binary (Contents/MacOS or portable binary dir)
        executable_dir().join("loader_binaries"),
        // Beside .app / portable root
        companion_dir().join("loader_binaries"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dependencies/loader_binaries"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("loader_binaries"),
    ];

    #[cfg(target_os = "macos")]
    if let Some(bundle) = app_bundle_root() {
        candidates.insert(0, bundle.join("Contents/Resources/loader_binaries"));
        candidates.insert(1, bundle.join("Contents/MacOS/loader_binaries"));
        if let Some(parent) = bundle.parent() {
            candidates.push(parent.join("loader_binaries"));
        }
    }

    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    companion_dir().join("loader_binaries")
}

pub fn loader_path(filename: &str) -> Option<PathBuf> {
    let p = loader_binaries_dir().join(filename);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

#[allow(dead_code)]
pub fn exists_beside(name: impl AsRef<Path>) -> bool {
    companion_dir().join(name).is_file()
}
