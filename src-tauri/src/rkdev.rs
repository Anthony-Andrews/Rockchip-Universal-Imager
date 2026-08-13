//! Spawn and control external C++ `rkdeveloptool` (std::process + reader thread).

use std::fs;
use std::io::{BufRead, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::logging;
use crate::paths;

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub exit_code: i32,
    pub was_cancelled: bool,
    pub error_message: String,
}

/// `["-l", "0x<loc>"]` for a target device, else empty. Prepended to a
/// rkdeveloptool invocation so per-device commands target one board when several
/// are attached (requires the patched rkdeveloptool with -l support). Passed
/// explicitly per call so independent operations can run concurrently on
/// different devices. LocationID = (bus << 8) | port, never 0 for a real device.
fn selector_prefix(location: Option<u32>) -> Vec<String> {
    match location {
        Some(loc) if loc != 0 => vec!["-l".to_string(), format!("0x{loc:x}")],
        _ => Vec::new(),
    }
}

fn with_selector(location: Option<u32>, args: &[&str]) -> Vec<String> {
    let mut full = selector_prefix(location);
    full.extend(args.iter().map(|s| s.to_string()));
    full
}

// Child-process enumeration via `ld`. Only used on Windows (which has no libusb
// hotplug context); macOS/Linux enumerate in-process — see usb::list_devices.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub location: u32,
    pub vid: u16,
    pub pid: u16,
    /// "Maskrom" | "Loader" | "Unknown" (as reported by `ld`).
    pub mode: String,
}

/// Enumerate every attached rockusb device via `ld`. Runs WITHOUT the -l
/// selector (we want the full list, not one device), so it never depends on the
/// current selection.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn list_devices() -> Vec<DeviceInfo> {
    let (_res, out) = capture(None, &["ld".to_string()], Some(PROBE_TIMEOUT));
    // e.g. "DevNo=1\tVid=0x2207,Pid=0x350b,LocationID=0x1\tMaskrom"
    let Ok(re) = regex::Regex::new(
        r"(?i)Vid=0x([0-9a-f]+),Pid=0x([0-9a-f]+),LocationID=0x([0-9a-f]+)\s+(\S+)",
    ) else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for line in out.lines() {
        if let Some(c) = re.captures(line) {
            devices.push(DeviceInfo {
                vid: u16::from_str_radix(&c[1], 16).unwrap_or(0),
                pid: u16::from_str_radix(&c[2], 16).unwrap_or(0),
                location: u32::from_str_radix(&c[3], 16).unwrap_or(0),
                mode: c[4].to_string(),
            });
        }
    }
    devices
}

fn strip_ansi(line: &str) -> String {
    if !line.contains('\u{1b}') {
        return line.to_string();
    }
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn is_progress_line(line: &str) -> bool {
    line.contains('%') || (line.contains("total") && line.contains("current"))
}

fn emit_lines(buffer: &mut String, on_line: &dyn Fn(String), location: Option<u32>) {
    loop {
        let Some(pos) = buffer.find(['\r', '\n']) else {
            break;
        };
        let line = buffer[..pos].to_string();
        let mut erase = pos + 1;
        if buffer.as_bytes().get(pos) == Some(&b'\r') && buffer.as_bytes().get(pos + 1) == Some(&b'\n')
        {
            erase = pos + 2;
        }
        buffer.drain(..erase);
        let clean = strip_ansi(&line);
        if is_progress_line(&clean) {
            logging::write_progress(&rkdev_category(location), &clean);
        } else if !clean.is_empty() {
            logging::write_line(&format!("{} {clean}", rkdev_prefix(location)));
        }
        on_line(clean);
    }
}

/// Default timeout for short probes (`td`, `cs`, `rfi`, `rci`). Matches the
/// old C++ helper (5s). Without this, a stuck `cs` (common when probing a
/// storage target the loader doesn't have) hangs the UI forever.
pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Run rkdeveloptool to completion (optional kill-on-timeout), scoped to `location`.
pub fn run_sync(
    location: Option<u32>,
    args: &[&str],
    timeout: Option<std::time::Duration>,
) -> ProcessResult {
    run_sync_output_timeout(location, args, timeout).0
}

/// Like [`run_sync_output_timeout`] with [`PROBE_TIMEOUT`].
pub fn run_sync_output(location: Option<u32>, args: &[&str]) -> (ProcessResult, String) {
    run_sync_output_timeout(location, args, Some(PROBE_TIMEOUT))
}

fn format_command_line(args: &[impl AsRef<str>]) -> String {
    args.iter()
        .map(|a| a.as_ref())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Log category for a device-scoped call, e.g. "rkdev 0x101" (or "rkdev" when
/// not scoped), so every rkdeveloptool line in the log names its target device.
fn rkdev_category(location: Option<u32>) -> String {
    match location {
        Some(l) => format!("rkdev 0x{l:x}"),
        None => "rkdev".to_string(),
    }
}

fn rkdev_prefix(location: Option<u32>) -> String {
    format!("[{}]", rkdev_category(location))
}

/// Log the exact rkdeveloptool invocation (every path: sync probes + async tasks).
pub fn log_command(location: Option<u32>, args: &[impl AsRef<str>]) {
    logging::write_line(&format!(
        "{} $ rkdeveloptool {}",
        rkdev_prefix(location),
        format_command_line(args)
    ));
}

/// True if the output is an open/claim failure ("Creating Comm Object failed").
/// These are transient on macOS — the device is briefly not openable (just
/// enumerated, or another handle settling) — and usually succeed on retry.
pub fn is_open_failure(output: &str) -> bool {
    output.to_lowercase().contains("comm object failed")
}

/// How many times to re-run a command whose only problem was a transient open
/// failure, and how long to wait between tries.
const OPEN_RETRIES: u32 = 4;
const OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// Run rkdeveloptool, capture combined stdout/stderr, kill if `timeout` elapses.
/// Prepends the device selector (`-l <loc>`) so the probe targets `location`.
/// Retries transient "Creating Comm Object failed" open errors.
pub fn run_sync_output_timeout(
    location: Option<u32>,
    args: &[&str],
    timeout: Option<std::time::Duration>,
) -> (ProcessResult, String) {
    let full = with_selector(location, args);
    let mut last = capture(location, &full, timeout);
    let mut attempt = 1;
    while attempt < OPEN_RETRIES && is_open_failure(&last.1) {
        logging::write_line(&format!(
            "{} open failed, retrying ({}/{})",
            rkdev_prefix(location),
            attempt,
            OPEN_RETRIES - 1
        ));
        thread::sleep(OPEN_RETRY_DELAY);
        last = capture(location, &full, timeout);
        attempt += 1;
    }
    last
}

/// Core spawn+capture. `args` are passed to rkdeveloptool verbatim (already
/// including any selector prefix); `location` is only used to tag log lines with
/// the target device.
fn capture(
    location: Option<u32>,
    args: &[String],
    timeout: Option<std::time::Duration>,
) -> (ProcessResult, String) {
    let path = match paths::rkdeveloptool_path() {
        Ok(p) => p,
        Err(e) => {
            return (
                ProcessResult {
                    exit_code: -1,
                    was_cancelled: false,
                    error_message: e,
                },
                String::new(),
            );
        }
    };

    log_command(location, args);

    let mut cmd = Command::new(&path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                ProcessResult {
                    exit_code: -1,
                    was_cancelled: false,
                    error_message: format!("failed to start rkdeveloptool: {e}"),
                },
                String::new(),
            );
        }
    };

    let started = std::time::Instant::now();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let out_handle = thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = stdout.take() {
            let _ = r.read_to_string(&mut s);
        }
        s
    });
    let err_handle = thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut r) = stderr.take() {
            let _ = r.read_to_string(&mut s);
        }
        s
    });

    let (status_code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code().unwrap_or(-1), false),
            Ok(None) => {
                if let Some(t) = timeout {
                    if started.elapsed() > t {
                        let _ = child.kill();
                        let _ = child.wait();
                        break (-1, true);
                    }
                }
                thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                return (
                    ProcessResult {
                        exit_code: -1,
                        was_cancelled: false,
                        error_message: e.to_string(),
                    },
                    String::new(),
                );
            }
        }
    };

    let out = out_handle.join().unwrap_or_default();
    let err = err_handle.join().unwrap_or_default();
    let mut combined = out;
    if !err.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&err);
    }

    for line in combined.lines() {
        let clean = strip_ansi(line);
        if !clean.is_empty() && !is_progress_line(&clean) {
            logging::write_line(&format!("{} {clean}", rkdev_prefix(location)));
        }
    }

    if timed_out {
        logging::write_line(&format!(
            "{} timed out after {:?} ({})",
            rkdev_prefix(location),
            timeout.unwrap_or_default(),
            args.join(" ")
        ));
        return (
            ProcessResult {
                exit_code: -1,
                was_cancelled: true,
                error_message: "rkdeveloptool timed out".into(),
            },
            combined,
        );
    }

    (
        ProcessResult {
            exit_code: status_code,
            was_cancelled: false,
            error_message: if status_code == 0 {
                String::new()
            } else {
                combined
                    .lines()
                    .last()
                    .unwrap_or("rkdeveloptool failed")
                    .to_string()
            },
        },
        combined,
    )
}

pub struct RkdevTask {
    cancelled: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl RkdevTask {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

/// Start rkdeveloptool on a background thread; stream lines via callback.
pub fn start(
    location: Option<u32>,
    args: Vec<String>,
    on_line: impl Fn(String) + Send + Sync + 'static,
    on_exit: impl Fn(ProcessResult) + Send + Sync + 'static,
) -> Result<Arc<RkdevTask>, String> {
    // Prepend the device selector so db/wl/rd/etc. target `location`.
    let args = {
        let mut full = selector_prefix(location);
        full.extend(args);
        full
    };
    let path = paths::rkdeveloptool_path()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    let task = Arc::new(RkdevTask {
        cancelled: cancelled.clone(),
        child: child_slot.clone(),
        join: Mutex::new(None),
    });

    log_command(location, &args);

    let handle = thread::spawn(move || {
        let mut command = Command::new(&path);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                on_exit(ProcessResult {
                    exit_code: -1,
                    was_cancelled: false,
                    error_message: format!("failed to start rkdeveloptool: {e}"),
                });
                return;
            }
        };

        if cancelled.load(Ordering::SeqCst) {
            let _ = child.kill();
            on_exit(ProcessResult {
                exit_code: -1,
                was_cancelled: true,
                error_message: String::new(),
            });
            return;
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *child_slot.lock().unwrap() = Some(child);

        let on_line = Arc::new(on_line);
        let cancelled_r = cancelled.clone();

        let out_thread = {
            let on_line = on_line.clone();
            thread::spawn(move || {
                if let Some(out) = stdout {
                    let mut reader = std::io::BufReader::new(out);
                    let mut buffer = String::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        if cancelled_r.load(Ordering::SeqCst) {
                            break;
                        }
                        match reader.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buffer.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                emit_lines(&mut buffer, &*on_line, location);
                            }
                            Err(_) => break,
                        }
                    }
                    if !buffer.is_empty() {
                        emit_lines(&mut buffer, &*on_line, location);
                        if !buffer.is_empty() {
                            on_line(strip_ansi(&buffer));
                        }
                    }
                }
            })
        };

        let err_thread = {
            let on_line = on_line.clone();
            thread::spawn(move || {
                if let Some(err) = stderr {
                    let reader = std::io::BufReader::new(err);
                    for line in reader.lines().flatten() {
                        let clean = strip_ansi(&line);
                        logging::write_line(&format!("{} {clean}", rkdev_prefix(location)));
                        on_line(clean);
                    }
                }
            })
        };

        let _ = out_thread.join();
        let _ = err_thread.join();

        let was_cancelled = cancelled.load(Ordering::SeqCst);
        let exit_code = {
            let mut guard = child_slot.lock().unwrap();
            if let Some(mut child) = guard.take() {
                match child.wait() {
                    Ok(s) => s.code().unwrap_or(if was_cancelled { -1 } else { -1 }),
                    Err(_) => -1,
                }
            } else {
                -1
            }
        };

        on_exit(ProcessResult {
            exit_code,
            was_cancelled,
            error_message: if was_cancelled || exit_code == 0 {
                String::new()
            } else {
                format!("rkdeveloptool failed with exit code {exit_code}")
            },
        });
    });

    *task.join.lock().unwrap() = Some(handle);
    Ok(task)
}

pub fn parse_progress_percent(line: &str) -> Option<i32> {
    let re = regex::Regex::new(r"([0-9]{1,3})%").ok()?;
    if let Some(c) = re.captures(line) {
        if let Ok(n) = c[1].parse::<i32>() {
            return Some(n.clamp(0, 100));
        }
    }
    let re2 = regex::Regex::new(r"(?i)total\s+(\d+)K?,\s*current\s+(\d+)K?").ok()?;
    if let Some(c) = re2.captures(line) {
        let total: i64 = c[1].parse().ok()?;
        let current: i64 = c[2].parse().ok()?;
        if total > 0 {
            return Some(((current * 100) / total).clamp(0, 100) as i32);
        }
    }
    None
}

// ----- GPT probes -----


const GPT_HEADER_SIGNATURE: u64 = 0x5452_4150_2049_4645; // "EFI PART" LE
const SECTOR_SIZE: u64 = 512;
const GPT_PROBE_SECTORS: u64 = 34;

static PROBE_ID: AtomicU64 = AtomicU64::new(0);

fn temp_probe_path() -> PathBuf {
    let id = PROBE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("rui_sector_probe_{id}.bin"))
}

fn read_le_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let slice = buf.get(offset..offset + 8)?;
    Some(u64::from_le_bytes(slice.try_into().ok()?))
}

fn read_le_u32(buf: &[u8], offset: usize) -> Option<u32> {
    let slice = buf.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

fn is_all_zero(buf: &[u8], offset: usize, size: usize) -> bool {
    buf.get(offset..offset + size)
        .map(|s| s.iter().all(|&b| b == 0))
        .unwrap_or(true)
}

pub fn read_sectors(location: Option<u32>, begin: u64, count: u64) -> Option<Vec<u8>> {
    let temp = temp_probe_path();
    let _ = fs::remove_file(&temp);
    let args = [
        "rl",
        &begin.to_string(),
        &count.to_string(),
        temp.to_str()?,
    ];
    let result = run_sync(location, &args, Some(PROBE_TIMEOUT));
    if result.exit_code != 0 || result.was_cancelled {
        let _ = fs::remove_file(&temp);
        return None;
    }
    let mut file = fs::File::open(&temp).ok()?;
    let mut buf = vec![0u8; (count * SECTOR_SIZE) as usize];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    let _ = fs::remove_file(&temp);
    Some(buf)
}

#[derive(Debug, Clone)]
pub struct GptInfo {
    pub last_used_lba: u64,
}

pub fn read_gpt_info(location: Option<u32>) -> Option<GptInfo> {
    let buf = read_sectors(location, 0, GPT_PROBE_SECTORS)?;
    if buf.len() < (SECTOR_SIZE * 2) as usize {
        return None;
    }
    let header_offset = SECTOR_SIZE as usize;
    let signature = read_le_u64(&buf, header_offset)?;
    if signature != GPT_HEADER_SIGNATURE {
        return None;
    }
    let entry_lba = read_le_u64(&buf, header_offset + 72)?;
    let entry_count = read_le_u32(&buf, header_offset + 80)?;
    let entry_size = read_le_u32(&buf, header_offset + 84)?;
    if entry_size == 0 || entry_count == 0 {
        return None;
    }
    let entries_offset = (entry_lba as usize) * (SECTOR_SIZE as usize);
    let mut max_ending: Option<u64> = None;
    for i in 0..entry_count {
        let entry_offset = entries_offset + (i as usize) * (entry_size as usize);
        if entry_offset + entry_size as usize > buf.len() {
            break;
        }
        if is_all_zero(&buf, entry_offset, 16) {
            continue;
        }
        let ending = read_le_u64(&buf, entry_offset + 40)?;
        max_ending = Some(max_ending.map_or(ending, |m| m.max(ending)));
    }
    max_ending.map(|last_used_lba| GptInfo { last_used_lba })
}

fn looks_blank(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return false;
    }
    let first = buf[0];
    buf.iter().all(|&b| b == first)
}

/// Binary-search approximate used-sector boundary (matches C++ behavior).
pub fn find_used_sector_boundary(location: Option<u32>, total_sectors: u64) -> u64 {
    const PRECISION: u64 = 204_800; // 0.1 GiB @ 512 B
    const PROBE: u64 = 16;

    let mut lo = 0u64;
    let mut hi = total_sectors;
    while hi - lo > PRECISION {
        let mid = lo + (hi - lo) / 2;
        let blank = read_sectors(location, mid, PROBE)
            .map(|b| looks_blank(&b))
            .unwrap_or(false);
        if blank {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    if lo == 0 {
        0
    } else {
        (lo + hi) / 2
    }
}

