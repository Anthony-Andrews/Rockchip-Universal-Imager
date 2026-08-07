let lastInfo = "";
let lastStatus = "disconnected";
let lastSoc = "";
let driverInstallRunning = false;
let flashRunning = false;
let selectedImagePath = "";
let logVisible = false;
let advancedVisible = false;
let logLoaded = false;
let logCleared = false;
let calculatingUsedSpace = false;
let quitPromptShowing = false;
let storageTargets = null;
const driverDeviceName = "Rockchip Bootloader Device";

const statusDot = document.getElementById("statusDot");
const statusText = document.getElementById("statusText");
const storageInfo = document.getElementById("storageInfo");
const storageSelector = document.getElementById("storageSelector");
const infoIcon = document.getElementById("infoIcon");
const driverStatus = document.getElementById("driverStatus");
const installDriver = document.getElementById("installDriver");
const flashStatus = document.getElementById("flashStatus");
const errorStatus = document.getElementById("errorStatus");
const flashProgress = document.getElementById("flashProgress");
const connectDevice = document.getElementById("connectDevice");
const selectImage = document.getElementById("selectImage");
const flashImage = document.getElementById("flashImage");
const eraseStorage = document.getElementById("eraseStorage");
const secureEraseStorage = document.getElementById("secureEraseStorage");
const toggleAdvanced = document.getElementById("toggleAdvanced");
const advancedPanel = document.getElementById("advancedPanel");
const backupStorage = document.getElementById("backupStorage");
const calculateUsed = document.getElementById("calculateUsed");
const cancelFlash = document.getElementById("cancelFlash");
const selectedImage = document.getElementById("selectedImage");
const toggleLog = document.getElementById("toggleLog");
const logPanel = document.getElementById("logPanel");
const liveLog = document.getElementById("liveLog");
const copyLog = document.getElementById("copyLog");
const clearLog = document.getElementById("clearLog");
const openLogDir = document.getElementById("openLogDir");
const confirmModal = document.getElementById("confirmModal");
const confirmMessage = document.getElementById("confirmMessage");
const confirmOkBtn = document.getElementById("confirmOkBtn");
const confirmCancelBtn = document.getElementById("confirmCancelBtn");
const alertModal = document.getElementById("alertModal");
const alertMessage = document.getElementById("alertMessage");
const alertOkBtn = document.getElementById("alertOkBtn");
// Tauri bridge: camelCase method names map to snake_case command ids.
// Argument object keys stay camelCase — Tauri 2 defaults to camelCase IPC
// keys (image_path → imagePath), not snake_case.
function createTauriApi() {
    const core = window.__TAURI__ && window.__TAURI__.core;
    if (!core || !core.invoke) {
        return null;
    }
    const invoke = core.invoke.bind(core);
    const names = [
        "uiReady",
        "getLogContents",
        "openLogDirectory",
        "getPlatform",
        "getDependencyStatus",
        "getDeviceAccessInfo",
        "installDeviceAccess",
        "selectImageFile",
        "selectBackupDestination",
        "flashBootloader",
        "disconnectDevice",
        "flashImage",
        "eraseStorage",
        "secureEraseStorage",
        "backupStorage",
        "cancelFlash",
        "forceCloseWindow",
        "getStorageInfo",
        "getStorageTargets",
        "selectStorage",
        "calculateUsedSpace",
    ];
    const api = {};
    for (const name of names) {
        const cmd = name.replace(/[A-Z]/g, (ch) => "_" + ch.toLowerCase());
        api[name] = (...args) => {
            if (args.length === 0) {
                return invoke(cmd);
            }
            if (name === "flashImage") {
                return invoke(cmd, { imagePath: args[0] });
            }
            if (name === "selectStorage") {
                return invoke(cmd, { storage: args[0] });
            }
            if (name === "backupStorage") {
                return invoke(cmd, { destPath: args[0], force: !!args[1] });
            }
            if (name === "installDeviceAccess") {
                return invoke(cmd, { deviceName: args[0] || "" });
            }
            return invoke(cmd, { args });
        };
    }
    return api;
}
const api = createTauriApi();

let currentOperation = null;
// Captured when an operation starts so completion messages name the storage
// target that was actually operated on, even if the selection changes later.
let currentOperationStorageLabel = "eMMC";
let dependencyWarning = "";
let runtimeError = "";

function storageLabel(storage) {
    switch (Number(storage)) {
    case 1:
        return "eMMC";
    case 2:
        return "SD card";
    case 9:
        return "SPI NOR";
    default:
        return "storage";
    }
}

// Tauri serializes #[serde(rename_all = "camelCase")] command results, so
// prefer camelCase and keep snake_case as a fallback for older hosts.
function pickField(obj, ...keys) {
    if (!obj) {
        return undefined;
    }
    for (const key of keys) {
        if (Object.prototype.hasOwnProperty.call(obj, key) && obj[key] !== undefined && obj[key] !== null) {
            return obj[key];
        }
    }
    return undefined;
}

function selectedStorageValue() {
    const selected = pickField(storageTargets, "selectedStorage", "selected_storage");
    if (selected) {
        return Number(selected);
    }
    return storageSelector ? Number(storageSelector.value) : 1;
}

function selectedStorageLabel() {
    return storageLabel(selectedStorageValue());
}

function basename(path) {
    if (!path) {
        return "";
    }
    const idx = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    return idx >= 0 ? path.slice(idx + 1) : path;
}

// Errors get their own line instead of sharing flashStatus with the progress
// animation, which used to overwrite them on the next status tick.
function showError(message) {
    runtimeError = message || "";
    errorStatus.textContent = runtimeError || dependencyWarning;
    errorStatus.style.color = "#e05b5b";
}

function setDependencyWarning(message) {
    dependencyWarning = message || "";
    errorStatus.textContent = runtimeError || dependencyWarning;
    errorStatus.style.color = "#e05b5b";
}

function storageBytesFromInfo(info) {
    if (!info) {
        return 0;
    }
    return Number(pickField(info, "storageBytes", "storage_bytes", "emmcBytes", "emmc_bytes") || 0);
}

function formatGiB(bytes) {
    const gib = Number(bytes || 0) / (1024 * 1024 * 1024);
    const digits = gib >= 100 ? 0 : (gib >= 10 ? 1 : 2);
    return gib.toFixed(digits) + " GiB";
}

// Storage line state: sizes display as GiB with the raw byte counts in the
// tooltip. null means unknown/not yet read.
let storageTotalBytes = null;
let storageUsedBytes = null;

function renderStorageInfoLine() {
    const storage = selectedStorageValue();
    // Device size and used space are shown independently: whichever is known
    // renders as GiB with its exact byte count in the tooltip, so a computed
    // used-space value is never hidden just because the total isn't known yet.
    const parts = [];
    const titleParts = [];

    if (storageTotalBytes !== null) {
        parts.push(formatGiB(storageTotalBytes));
        titleParts.push(storageTotalBytes.toLocaleString() + " bytes total");
    } else {
        parts.push("unknown");
    }

    if (calculatingUsedSpace) {
        parts.push("Calculating...");
    } else if (storageUsedBytes !== null) {
        parts.push("Used: " + formatGiB(storageUsedBytes));
        titleParts.push(storageUsedBytes.toLocaleString() + " bytes used");
    }

    storageInfo.textContent = storageLabel(storage) + ": " + parts.join("  ·  ");
    storageInfo.title = titleParts.join("  ·  ");
}

function storageTargetAvailable(targets, storage) {
    switch (Number(storage)) {
    case 1:
        return !!(pickField(targets, "emmcAvailable", "emmc_available"));
    case 2:
        return !!(pickField(targets, "sdAvailable", "sd_available"));
    case 9:
        return !!(pickField(targets, "spinorAvailable", "spinor_available"));
    default:
        return false;
    }
}

function availableStorageEntries(targets) {
    if (!targets || !targets.success) {
        return [];
    }
    const entries = [];
    if (storageTargetAvailable(targets, 1)) {
        entries.push({ value: 1, label: "eMMC" });
    }
    if (storageTargetAvailable(targets, 2)) {
        entries.push({ value: 2, label: "SD card" });
    }
    if (storageTargetAvailable(targets, 9)) {
        entries.push({ value: 9, label: "SPI NOR" });
    }
    return entries;
}

function renderStorageSelector() {
    if (!storageSelector) {
        return;
    }

    const entries = availableStorageEntries(storageTargets);
    const enabledValues = new Set(entries.map((entry) => String(entry.value)));

    for (const option of storageSelector.options) {
        const available = enabledValues.has(option.value);
        option.disabled = !available;
        option.title = available ? "" : "not detected";
    }

    const selectedRaw = pickField(storageTargets, "selectedStorage", "selected_storage");
    const selected = selectedRaw ? String(selectedRaw) : storageSelector.value;
    if (enabledValues.has(selected)) {
        storageSelector.value = selected;
    }
    const noStorageDevices = !!(storageTargets && !storageTargets.success);
    storageSelector.disabled = flashRunning || (lastStatus !== "connected" && !noStorageDevices);
}

async function refreshStorageTargets() {
    if (!api || !api.getStorageTargets) {
        return;
    }
    try {
        const targets = await api.getStorageTargets();
        storageTargets = targets || null;
    } catch (error) {
        storageTargets = null;
    }
    if (storageTargets && !storageTargets.success) {
        flashStatus.textContent = "no storage devices detected";
    } else if (flashStatus.textContent === "no storage devices detected") {
        flashStatus.textContent = "";
    }
    renderStorageSelector();
    render();
}

// Both the "connected" status transition and a completed flash task ask for
// the same storage refresh at nearly the same moment; a second caller while
// one is in flight would just duplicate the rkdeveloptool round-trips, so
// coalesce them onto the in-flight promise.
let storageRefreshInFlight = null;
function scheduleStorageRefresh() {
    if (storageRefreshInFlight) {
        return storageRefreshInFlight;
    }
    storageRefreshInFlight = refreshStorageTargets()
        .then(refreshStorageInfo)
        .finally(() => {
            storageRefreshInFlight = null;
        });
    return storageRefreshInFlight;
}

function showConfirm(message) {
    return new Promise((resolve) => {
        confirmMessage.textContent = message;
        confirmModal.style.display = "flex";
        const cleanup = (result) => {
            confirmModal.style.display = "none";
            confirmOkBtn.removeEventListener("click", onOk);
            confirmCancelBtn.removeEventListener("click", onCancel);
            document.removeEventListener("keydown", onKey);
            resolve(result);
        };
        const onOk = () => cleanup(true);
        const onCancel = () => cleanup(false);
        const onKey = (event) => {
            if (event.key === "Escape") {
                event.preventDefault();
                onCancel();
            }
        };
        confirmOkBtn.addEventListener("click", onOk);
        confirmCancelBtn.addEventListener("click", onCancel);
        document.addEventListener("keydown", onKey);
        // Focus the safe choice: Enter/Space activate it, Tab reaches
        // Confirm, Escape backs out.
        confirmCancelBtn.focus();
    });
}

function showAlert(message) {
    return new Promise((resolve) => {
        alertMessage.textContent = message;
        alertModal.style.display = "flex";
        const cleanup = () => {
            alertModal.style.display = "none";
            alertOkBtn.removeEventListener("click", cleanup);
            document.removeEventListener("keydown", onKey);
            resolve();
        };
        const onKey = (event) => {
            if (event.key === "Escape") {
                event.preventDefault();
                cleanup();
            }
        };
        alertOkBtn.addEventListener("click", cleanup);
        document.addEventListener("keydown", onKey);
        alertOkBtn.focus();
    });
}

function render() {
    const connected = lastStatus === "connected";
    const detected = lastStatus === "detected";
    const toolMissing = lastStatus === "tool_missing";
    const dependenciesMissing = dependencyWarning.length > 0;
    const noStorageDevices = !!(storageTargets && !storageTargets.success);
    statusDot.style.background = connected ? "#2fa84f" : (detected ? "#2a6fd9" : (toolMissing ? "#d9822b" : "#a33"));
    const socSuffix = (lastSoc && (connected || detected)) ? " (" + lastSoc + ")" : "";
    statusText.textContent = toolMissing ? "rkdeveloptool not found" : ((detected ? "detected" : lastStatus) + socSuffix);
    flashImage.disabled = flashRunning || !connected || !selectedImagePath || noStorageDevices || dependenciesMissing;
    flashImage.textContent = "Flash " + selectedStorageLabel();
    eraseStorage.disabled = flashRunning || !connected || noStorageDevices || dependenciesMissing;
    secureEraseStorage.disabled = flashRunning || !connected || noStorageDevices || dependenciesMissing;
    backupStorage.disabled = flashRunning || !connected || noStorageDevices || dependenciesMissing;
    backupStorage.textContent = "Backup " + selectedStorageLabel();
    calculateUsed.disabled = flashRunning || !connected || calculatingUsedSpace || noStorageDevices || dependenciesMissing;
    connectDevice.style.display = (connected || detected) ? "inline-block" : "none";
    connectDevice.textContent = connected ? "Disconnect" : "Connect";
    connectDevice.style.background = connected ? "#a33" : "#2a6fd9";
    connectDevice.disabled = flashRunning || noStorageDevices || dependenciesMissing;
    cancelFlash.style.display = flashRunning ? "inline-block" : "none";
    if (toggleAdvanced) {
        toggleAdvanced.disabled = noStorageDevices;
    }
    if (toggleLog) {
        toggleLog.disabled = noStorageDevices;
    }
    if (copyLog) {
        copyLog.disabled = noStorageDevices;
    }
    if (clearLog) {
        clearLog.disabled = noStorageDevices;
    }
    if (installDriver) {
        installDriver.disabled = driverInstallRunning || noStorageDevices;
    }
    if (connected || detected) {
        const friendly = connected ? "device connected" : "device detected — press Connect";
        infoIcon.title = lastInfo.trim() || friendly;
    } else if (toolMissing) {
        infoIcon.title = "rkdeveloptool is missing beside rockchip-universal-imager.app — keep it in the same folder as the app.";
        if (!driverInstallRunning) {
            driverStatus.textContent = "";
        }
    } else {
        infoIcon.title = "check usb";
        if (!driverInstallRunning) {
            driverStatus.textContent = "";
        }
    }
    if (storageSelector) {
        storageSelector.disabled = flashRunning || (lastStatus !== "connected" && !noStorageDevices);
        const selected = pickField(storageTargets, "selectedStorage", "selected_storage");
        if (selected) {
            storageSelector.value = String(selected);
        }
    }
    if (noStorageDevices && !flashRunning) {
        flashStatus.textContent = "no storage devices detected";
    }
    if (!connected) {
        storageInfo.textContent = "";
        storageInfo.title = "";
    }
}

function setDriverInstallRunning(running) {
    driverInstallRunning = running;
    if (installDriver) {
        installDriver.disabled = running;
    }
    if (running) {
        driverStatus.textContent = "Installing... (this may take a while)";
    }
}

let flashDotInterval = null;
let flashDotCount = 0;
let flashPercent = 0;
let flashBaseLabel = "Flashing";

function updateFlashStatusText() {
    const dots = ".".repeat(flashDotCount);
    // "Connecting" (bootloader download) doesn't report granular progress -
    // showing a static "0%" the whole time would look broken rather than
    // just quick, so only show the number for operations that actually
    // report one.
    const showPercent = currentOperation !== "connect";
    flashStatus.textContent = flashBaseLabel + dots + (showPercent ? " " + flashPercent + "%" : "");
}

function startFlashAnimation(label) {
    flashBaseLabel = label;
    flashDotCount = 0;
    flashPercent = 0;
    updateFlashStatusText();
    if (flashDotInterval) {
        clearInterval(flashDotInterval);
    }
    flashDotInterval = setInterval(() => {
        flashDotCount = (flashDotCount + 1) % 4;
        updateFlashStatusText();
    }, 400);
}

function stopFlashAnimation() {
    if (flashDotInterval) {
        clearInterval(flashDotInterval);
        flashDotInterval = null;
    }
}

function setFlashRunning(running) {
    flashRunning = running;
    selectImage.disabled = running;
    if (running) {
        showError("");
    } else {
        stopFlashAnimation();
        flashStatus.title = "";
    }
    render();
}

const platformReady = (api && api.getPlatform)
    ? Promise.resolve(api.getPlatform()).catch(() => "")
    : Promise.resolve("");

async function refreshDependencyWarning() {
    if (!api || !api.getDependencyStatus) {
        setDependencyWarning("");
        return;
    }
    try {
        const status = await api.getDependencyStatus();
        setDependencyWarning(status && status.warning ? status.warning : "");
    } catch (error) {
        setDependencyWarning("Required dependency is missing - keep the application files together and reinstall if needed.");
    }
}

async function refreshDriverInfo() {
    if (!api || !api.getDeviceAccessInfo) {
        return;
    }
    try {
        const info = await api.getDeviceAccessInfo();
        if (!info || info.kind === "none") {
            return;
        }
        if (info.kind === "windows_driver") {
            if (!pickField(info, "deviceRelevant", "device_relevant")) {
                driverStatus.textContent = info.error || "device not found";
                return;
            }
            if (info.ready) {
                driverStatus.textContent = "Driver: " + (info.detail || "libusb-win32");
            } else {
                driverStatus.textContent = info.error || ("Driver: " + (info.detail || "unknown"));
            }
            return;
        }
        if (info.kind === "linux_udev") {
            driverStatus.textContent = info.ready
                ? "udev rules: installed"
                : (info.error || "udev rules: not installed — flashing may need root");
        }
    } catch (error) {
        // ignore; status line is best-effort
    }
}

async function refreshStorageInfo() {
    if (!api || !api.getStorageInfo) {
        return;
    }
    storageUsedBytes = null;
    const selectedStorage = selectedStorageValue();
    // getStorageInfo reports whatever target is currently selected (via rfi).
    // SD capacity is not reliable from the loader — always show "unknown".
    const available = storageTargets && storageTargets.success
        && storageTargetAvailable(storageTargets, selectedStorage);
    if (!available || selectedStorage === 2) {
        storageTotalBytes = null;
        renderStorageInfoLine();
        return;
    }
    const info = await api.getStorageInfo();
    storageTotalBytes = info.success ? storageBytesFromInfo(info) : null;
    renderStorageInfoLine();
}

window.updateDeviceStatus = (status) => {
    const changed = status !== lastStatus;
    lastStatus = status;
    render();
    if (changed && status === "connected") {
        refreshDriverInfo();
        scheduleStorageRefresh();
    }
};

window.updateDeviceInfo = (info) => {
    lastInfo = info || "";
    render();
};

window.updateDeviceSoc = (soc) => {
    lastSoc = soc || "";
    render();
};

// Fired by the background probe once it has finished checking the storage
// targets the quick connect-time probe skipped.
window.onStorageTargetsUpdated = () => {
    refreshStorageTargets();
};

const maxLiveLogLines = 5000;
let liveLogLineCount = 0;
let liveLogLastLineStart = 0;

function resetLiveLogTracking() {
    const value = liveLog.value;
    liveLogLineCount = 0;
    for (let i = 0; i < value.length; i += 1) {
        if (value[i] === "\n") {
            liveLogLineCount += 1;
        }
    }
    liveLogLastLineStart = value.length === 0 ? 0 : value.lastIndexOf("\n", value.length - 2) + 1;
}

function liveLogAtBottom() {
    return liveLog.scrollTop + liveLog.clientHeight >= liveLog.scrollHeight - 8;
}

window.appendLiveLog = (line, replaceLast) => {
    if (!line) {
        return;
    }
    // Only chase the tail if the user is already there - scrolling up to
    // read older output mustn't be yanked back down by new lines.
    const atBottom = liveLogAtBottom();
    if (replaceLast && liveLogLineCount > 0) {
        // Consecutive progress updates replace the previous line, mirroring
        // exactly what the log file does.
        liveLog.value = liveLog.value.slice(0, liveLogLastLineStart) + line + "\n";
    } else {
        liveLogLastLineStart = liveLog.value.length;
        liveLog.value += line + "\n";
        liveLogLineCount += 1;
        if (liveLogLineCount > maxLiveLogLines) {
            // Trim a tenth at a time so the O(buffer) cut is amortized rather
            // than paid on every appended line.
            const drop = Math.floor(maxLiveLogLines / 10);
            let cut = 0;
            for (let i = 0; i < drop; i += 1) {
                const next = liveLog.value.indexOf("\n", cut);
                if (next < 0) {
                    break;
                }
                cut = next + 1;
            }
            liveLog.value = liveLog.value.slice(cut);
            liveLogLineCount -= drop;
            liveLogLastLineStart = Math.max(0, liveLogLastLineStart - cut);
        }
    }
    if (atBottom) {
        liveLog.scrollTop = liveLog.scrollHeight;
    }
};

toggleLog.addEventListener("click", () => {
    logVisible = !logVisible;
    logPanel.style.display = logVisible ? "block" : "none";
    toggleLog.textContent = logVisible ? "Hide Log" : "Show Log";
    if (logVisible && api && api.getLogContents && !logLoaded && !logCleared) {
        api.getLogContents().then(result => {
            try {
                liveLog.value = (result && result.text) ? result.text : "";
                resetLiveLogTracking();
                liveLog.scrollTop = liveLog.scrollHeight;
                logLoaded = true;
            } catch (e) {
                liveLog.value = "";
                resetLiveLogTracking();
            }
        });
    }
});

toggleAdvanced.addEventListener("click", () => {
    advancedVisible = !advancedVisible;
    advancedPanel.style.display = advancedVisible ? "block" : "none";
    toggleAdvanced.textContent = advancedVisible ? "Hide Advanced Options" : "Show Advanced Options";
});

copyLog.addEventListener("click", async () => {
    const text = liveLog.value || "";
    if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(text);
    } else {
        liveLog.select();
        document.execCommand("copy");
        liveLog.setSelectionRange(0, 0);
    }
});

clearLog.addEventListener("click", () => {
    liveLog.value = "";
    resetLiveLogTracking();
    logCleared = true;
});

if (openLogDir) {
    openLogDir.addEventListener("click", async () => {
        if (!api || !api.openLogDirectory) {
            return;
        }
        const result = await api.openLogDirectory();
        if (result && !result.success) {
            showError(result.error || "could not open log folder");
        }
    });
}

function applyImageSelection(path, sizeBytes) {
    selectedImagePath = path;
    selectedImage.textContent = basename(path) + " (" + formatGiB(sizeBytes) + ")";
    selectedImage.title = path + "\n" + Number(sizeBytes || 0).toLocaleString() + " bytes";
    flashStatus.textContent = "image selected";
    showError("");
    render();
}

selectImage.addEventListener("click", async () => {
    if (!api || !api.selectImageFile) {
        showError("file picker unavailable");
        return;
    }
    const result = await api.selectImageFile();
    if (!result.success) {
        flashStatus.textContent = result.error || "file picker canceled";
        return;
    }
    applyImageSelection(result.path, pickField(result, "sizeBytes", "size_bytes") || 0);
});

// Native host (where supported) delivers OS file drops of a single .img here.
window.onImageFileDropped = (result) => {
    if (!result || !result.success || !result.path) {
        return;
    }
    if (flashRunning) {
        return;
    }
    applyImageSelection(result.path, pickField(result, "sizeBytes", "size_bytes") || 0);
};

// Native drag-drop hover feedback. Fired from Rust because HTML5 drag events
// don't reach the webview while native drag-drop is enabled.
window.onImageDragState = (state) => {
    const overlay = document.getElementById("dropOverlay");
    if (!overlay) {
        return;
    }
    // Never show the overlay mid-flash - a drop would be ignored anyway.
    const active = !!(state && state.active) && !flashRunning;
    const reject = active && !(state && state.valid);
    overlay.classList.toggle("active", active);
    overlay.classList.toggle("reject", reject);
    const text = document.getElementById("dropOverlayText");
    if (text) {
        text.textContent = reject ? "Drop a single .img file" : "Drop .img to select";
    }
};

// Anything the native drop overlay doesn't claim must not fall through to
// the webview's default behavior (navigating to the dropped file).
window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => event.preventDefault());

async function startBackgroundOp(label, invokeStart) {
    setFlashRunning(true);
    flashProgress.value = 0;
    try {
        const result = await invokeStart();
        if (!result || !result.started) {
            setFlashRunning(false);
            flashStatus.textContent = "";
            showError((result && result.error) || (label + " failed"));
            return false;
        }
        return true;
    } catch (error) {
        setFlashRunning(false);
        flashStatus.textContent = "";
        showError((error && error.message) || String(error) || (label + " failed"));
        return false;
    }
}

flashImage.addEventListener("click", async () => {
    if (!api || !api.flashImage) {
        showError("flash unavailable");
        return;
    }
    if (flashRunning) {
        return;
    }
    currentOperation = "image";
    currentOperationStorageLabel = selectedStorageLabel();
    startFlashAnimation("Flashing " + basename(selectedImagePath));
    flashStatus.title = selectedImagePath;
    await startBackgroundOp("flash", () => api.flashImage(selectedImagePath));
});

connectDevice.addEventListener("click", async () => {
    const disconnecting = lastStatus === "connected";
    if (!api || (disconnecting ? !api.disconnectDevice : !api.flashBootloader)) {
        showError((disconnecting ? "disconnect" : "connect") + " unavailable");
        return;
    }
    if (flashRunning) {
        return;
    }
    currentOperation = disconnecting ? "disconnect" : "connect";
    currentOperationStorageLabel = selectedStorageLabel();
    startFlashAnimation(disconnecting ? "Disconnecting" : "Connecting");
    await startBackgroundOp(
        disconnecting ? "disconnect" : "connect",
        () => (disconnecting ? api.disconnectDevice() : api.flashBootloader())
    );
});

eraseStorage.addEventListener("click", async () => {
    if (!api || !api.eraseStorage) {
        showError("erase unavailable");
        return;
    }
    if (flashRunning) {
        return;
    }
    const label = selectedStorageLabel();
    const confirmed = await showConfirm(
        "This will erase the " + label + "'s partition table and flashed OS, leaving the device unbootable until " +
        "reflashed. Note: this is not a guaranteed secure wipe - depending on the device, old data may " +
        "still be physically recoverable afterward. Continue?"
    );
    if (!confirmed) {
        return;
    }
    currentOperation = "erase";
    currentOperationStorageLabel = label;
    startFlashAnimation("Erasing " + label);
    await startBackgroundOp("erase", () => api.eraseStorage());
});

secureEraseStorage.addEventListener("click", async () => {
    if (!api || !api.secureEraseStorage) {
        showError("secure erase unavailable");
        return;
    }
    if (flashRunning) {
        return;
    }
    const label = selectedStorageLabel();
    const confirmed = await showConfirm(
        "This will overwrite the entire " + label + " with zeros, physically destroying all data including the " +
        "flashed OS. This takes significantly longer than Quick Erase (potentially 15-60+ minutes " +
        "depending on device size and transfer speed) but is a real guarantee rather than relying on the " +
        "device's own erase command. This cannot be undone. Continue?"
    );
    if (!confirmed) {
        return;
    }
    currentOperation = "secure_erase";
    currentOperationStorageLabel = label;
    startFlashAnimation("Secure erasing " + label);
    await startBackgroundOp("secure erase", () => api.secureEraseStorage());
});

backupStorage.addEventListener("click", async () => {
    if (!api || !api.selectBackupDestination || !api.backupStorage) {
        showError("backup unavailable");
        return;
    }
    if (flashRunning) {
        return;
    }
    let picked;
    try {
        picked = await api.selectBackupDestination();
    } catch (error) {
        showError((error && error.message) || String(error) || "backup destination failed");
        return;
    }
    if (!picked.success) {
        return;
    }

    let result;
    try {
        result = await api.backupStorage(picked.path, false);
        if (!result.started && pickField(result, "needsConfirmation", "needs_confirmation")) {
            const confirmed = await showConfirm(result.message);
            if (!confirmed) {
                return;
            }
            result = await api.backupStorage(picked.path, true);
        }
    } catch (error) {
        showError((error && error.message) || String(error) || "backup failed");
        return;
    }

    if (!result.started) {
        showError(result.message || "backup failed");
        return;
    }

    const label = selectedStorageLabel();
    currentOperation = "backup";
    currentOperationStorageLabel = label;
    setFlashRunning(true);
    flashProgress.value = 0;
    startFlashAnimation("Backing up " + label + " → " + basename(picked.path));
    flashStatus.title = picked.path;
});

calculateUsed.addEventListener("click", async () => {
    if (!api || !api.calculateUsedSpace) {
        return;
    }
    if (calculatingUsedSpace || flashRunning) {
        return;
    }
    calculatingUsedSpace = true;
    renderStorageInfoLine();
    render();
    const result = await api.calculateUsedSpace();
    calculatingUsedSpace = false;
    if (!result.success) {
        storageUsedBytes = null;
        renderStorageInfoLine();
        showError(result.error || "calculate failed");
        render();
        return;
    }
    storageUsedBytes = Number(pickField(result, "usedBytes", "used_bytes") || 0);
    renderStorageInfoLine();
    render();
});

if (storageSelector) {
    storageSelector.addEventListener("change", async () => {
        if (!api || !api.selectStorage) {
            return;
        }
        const nextStorage = Number(storageSelector.value);
        const previousSelected = pickField(storageTargets, "selectedStorage", "selected_storage");
        const previousStorage = previousSelected
            ? String(previousSelected)
            : storageSelector.value;
        const result = await api.selectStorage(nextStorage);
        if (!result.started) {
            storageSelector.value = previousStorage;
            showError(result.error || "storage selection failed");
            return;
        }
        if (storageTargets) {
            storageTargets.selectedStorage = nextStorage;
            storageTargets.selected_storage = nextStorage;
        }
        calculatingUsedSpace = false;
        await refreshStorageInfo();
        render();
    });
}

cancelFlash.addEventListener("click", async () => {
    if (!flashRunning || !api || !api.cancelFlash) {
        return;
    }
    const confirmed = await showConfirm(
        "Canceling now will leave the current operation incomplete and may leave the device in an unusable state. Continue?"
    );
    if (!confirmed) {
        return;
    }
    try {
        const result = await api.cancelFlash();
        // Host should fire onFlashComplete; if cancel couldn't attach to a task
        // and also failed to unlock, clear the UI so Cancel is never a dead end.
        if (result && result.started === false && flashRunning) {
            setFlashRunning(false);
            flashStatus.textContent = "Cancel failed";
            showError(result.error || "no flash in progress");
        }
    } catch (error) {
        setFlashRunning(false);
        showError((error && error.message) || String(error) || "cancel failed");
    }
});

async function initDriverInstallUi() {
    if (!installDriver) {
        return;
    }
    if (!api || !api.getDeviceAccessInfo || !api.installDeviceAccess) {
        installDriver.style.display = "none";
        driverStatus.style.display = "none";
        return;
    }
    let kind = "none";
    try {
        const info = await api.getDeviceAccessInfo();
        kind = (info && info.kind) || "none";
    } catch (error) {
        kind = "none";
    }
    if (kind === "none") {
        installDriver.style.display = "none";
        driverStatus.style.display = "none";
        return;
    }
    if (kind === "linux_udev") {
        installDriver.textContent = "Install udev rules";
    } else if (kind === "windows_driver") {
        installDriver.textContent = "Install libusb-win32";
    }
    refreshDriverInfo();
    installDriver.addEventListener("click", async () => {
        if (driverInstallRunning) {
            return;
        }
        setDriverInstallRunning(true);
        if (kind === "linux_udev") {
            driverStatus.textContent = "Installing udev rules... (system authorization required)";
        }
        const result = await api.installDeviceAccess(driverDeviceName);
        if (!result.started) {
            setDriverInstallRunning(false);
            driverStatus.textContent = result.error || "install already in progress";
        }
    });
}

initDriverInstallUi();

window.onDriverInstallComplete = (result) => {
    setDriverInstallRunning(false);
    if (!result || !result.success) {
        driverStatus.textContent = (result && result.error) || "install failed";
    } else {
        driverStatus.textContent = "installed";
    }
    refreshDriverInfo();
};

window.onFlashComplete = async (result) => {
    const operation = currentOperation;
    currentOperation = null;
    setFlashRunning(false);

    const storageLbl = currentOperationStorageLabel || "eMMC";
    const label = operation === "erase" ? "Erase " + storageLbl
        : operation === "secure_erase" ? "Secure Erase"
        : operation === "connect" ? "Connect"
        : operation === "disconnect" ? "Disconnect"
        : operation === "backup" ? "Backup " + storageLbl
        : "Flash " + storageLbl;

    if (result && result.cancelled) {
        flashProgress.value = 0;
        flashStatus.textContent = label + " canceled";
        await showAlert(label + " was canceled.");
        return;
    }
    if (!result || !result.success) {
        flashProgress.value = 0;
        const error = (result && result.error) || "flash failed";
        flashStatus.textContent = label + " failed";
        showError(error);
        await showAlert(label + " failed: " + error);
        return;
    }

    if (operation === "connect") {
        // The bootloader download is an internal step, not a user-facing
        // "flash" — the status dot turning green (once polling picks up
        // Loader mode) is the only feedback needed for a successful connect.
        flashStatus.textContent = "";
    } else if (operation === "disconnect") {
        flashStatus.textContent = "Disconnected";
    } else if (operation === "erase") {
        flashStatus.textContent = "Erase completed";
        await showAlert("Erase " + storageLbl + " completed successfully.");
    } else if (operation === "secure_erase") {
        flashStatus.textContent = "Secure erase completed";
        await showAlert("Secure Erase completed successfully - the entire " + storageLbl + " has been overwritten with zeros.");
    } else if (operation === "backup") {
        flashStatus.textContent = "Backup completed";
        await showAlert("Backup " + storageLbl + " completed successfully.");
    } else {
        flashStatus.textContent = "Flash completed";
        await showAlert("Flash " + storageLbl + " completed successfully.");
    }
    if (operation !== "disconnect" && lastStatus === "connected") {
        scheduleStorageRefresh();
    }
};

window.updateFlashProgress = (percent) => {
    const value = Math.max(0, Math.min(100, percent || 0));
    flashProgress.value = value;
    flashPercent = value;
    updateFlashStatusText();
};

window.onQuitDuringOperation = async () => {
    // The native window won't actually close on its own while a flash/erase/
    // backup is running (see window::event::close on the C++ side) - closing
    // mid-operation can leave the storage half-written or the device stuck
    // needing a reflash. Ask first; forceCloseWindow only fires if the user
    // confirms, and re-closing at that point is allowed through.
    if (quitPromptShowing) {
        return;
    }
    quitPromptShowing = true;
    try {
        const ok = await showConfirm(
            "A flash, erase, or backup is still in progress. Quitting now may leave the storage " +
            "partially written or the device needing to be reflashed. Quit anyway?"
        );
        if (ok && api && api.forceCloseWindow) {
            api.forceCloseWindow();
        }
    } finally {
        quitPromptShowing = false;
    }
};

window.addEventListener("load", () => {
    setTimeout(() => {
        refreshDependencyWarning();
        if (api && api.uiReady) {
            api.uiReady();
        }
    }, 0);
});

render();
