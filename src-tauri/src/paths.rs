//! Resolve app / companion / resource directories.
//!
//! Release bundles (Tauri bundler, see `tauri.conf.json > bundle`):
//! - `rkdeveloptool` is a Tauri sidecar and lands *beside the executable*
//!   (`Contents/MacOS` on macOS, `/usr/bin` for .deb/.rpm/AppImage, the
//!   install dir on Windows).
//! - `loader_binaries/` is a bundle resource and lands in Tauri's resource
//!   dir (`Contents/Resources`, `/usr/lib/<product name>`, or the install dir
//!   on Windows). The app records that dir at startup via
//!   [`set_bundle_resource_dir`].
//!
//! The legacy portable layout (companions next to the binary / next to the
//! `.app`) and the Nix layout (everything in `$out/bin`) are still accepted.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Tauri's resolved resource directory, recorded once from the `setup` hook.
static BUNDLE_RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Record Tauri's resolved resource directory (`app.path().resource_dir()`).
/// Bundles place `loader_binaries/` there. Safe to call more than once; only
/// the first value is kept.
pub fn set_bundle_resource_dir(dir: PathBuf) {
    let _ = BUNDLE_RESOURCE_DIR.set(dir);
}

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

/// loader_binaries in the Tauri resource dir, next to the app, or from the
/// repo during dev.
pub fn loader_binaries_dir() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Tauri bundle resource dir (Contents/Resources, /usr/lib/<product>,
    // $APPDIR/usr/lib/<product>, or the Windows install dir).
    if let Some(dir) = BUNDLE_RESOURCE_DIR.get() {
        candidates.push(dir.join("loader_binaries"));
    }

    #[cfg(target_os = "macos")]
    if let Some(bundle) = app_bundle_root() {
        candidates.push(bundle.join("Contents/Resources/loader_binaries"));
        candidates.push(bundle.join("Contents/MacOS/loader_binaries"));
    }

    candidates.extend([
        // Bundled inside .app (preferred on macOS)
        resource_dir().join("loader_binaries"),
        // Next to binary (Contents/MacOS, Nix $out/bin, portable binary dir)
        executable_dir().join("loader_binaries"),
        // Beside .app / portable root
        companion_dir().join("loader_binaries"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dependencies/loader_binaries"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("loader_binaries"),
    ]);

    #[cfg(target_os = "macos")]
    if let Some(parent) = app_bundle_root().and_then(|b| b.parent().map(Path::to_path_buf)) {
        // Legacy: companions were placed beside the .app
        candidates.push(parent.join("loader_binaries"));
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
