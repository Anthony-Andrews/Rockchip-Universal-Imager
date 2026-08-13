// ---- Shared / global UI state ----
let selectedImagePath = "";
let selectedImageBytes = 0;
let devices = [];                 // latest DeviceEntry[] pushed from the host
let dependencyWarning = "";
let runtimeError = "";
let driverInstallRunning = false;
let quitPromptShowing = false;
let logVisible = false;
let logLoaded = false;
let logCleared = false;
const driverDeviceName = "Rockchip Bootloader Device";

// Per-device capacity/used cache, keyed by "location:storage".
const storageInfoCache = {};
// Per-device last-operation result message, keyed by location: {text, ok}.
const deviceResults = {};
// Locations that have been successfully flashed this session (→ show Reboot).
const deviceFlashed = {};

const errorStatus = document.getElementById("errorStatus");
const installDriver = document.getElementById("installDriver");
const driverStatus = document.getElementById("driverStatus");
const selectImage = document.getElementById("selectImage");
const selectedImage = document.getElementById("selectedImage");
const deviceList = document.getElementById("deviceList");
const noDevices = document.getElementById("noDevices");
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

// Tauri bridge: camelCase method names map to snake_case command ids. Every
// device command takes a numeric `location` (USB LocationID) first.
function createTauriApi() {
    const core = window.__TAURI__ && window.__TAURI__.core;
    if (!core || !core.invoke) {
        return null;
    }
    const invoke = core.invoke.bind(core);
    const call = (name, argObj) => {
        const cmd = name.replace(/[A-Z]/g, (ch) => "_" + ch.toLowerCase());
        return invoke(cmd, argObj);
    };
    return {
        uiReady: () => call("uiReady"),
        getLogContents: () => call("getLogContents"),
        openLogDirectory: () => call("openLogDirectory"),
        getPlatform: () => call("getPlatform"),
        getDependencyStatus: () => call("getDependencyStatus"),
        getDeviceAccessInfo: () => call("getDeviceAccessInfo"),
        installDeviceAccess: (name) => call("installDeviceAccess", { deviceName: name || "" }),
        selectImageFile: () => call("selectImageFile"),
        selectBackupDestination: () => call("selectBackupDestination"),
        forceCloseWindow: () => call("forceCloseWindow"),
        listDevices: () => call("listDevices"),
        flashBootloader: (location) => call("flashBootloader", { location }),
        disconnectDevice: (location) => call("disconnectDevice", { location }),
        rebootDevice: (location) => call("rebootDevice", { location }),
        flashImage: (location, imagePath) => call("flashImage", { location, imagePath }),
        eraseStorage: (location) => call("eraseStorage", { location }),
        secureEraseStorage: (location) => call("secureEraseStorage", { location }),
        backupStorage: (location, destPath, force) => call("backupStorage", { location, destPath, force: !!force }),
        cancelFlash: (location) => call("cancelFlash", { location }),
        getStorageInfo: (location) => call("getStorageInfo", { location }),
        getStorageTargets: (location) => call("getStorageTargets", { location }),
        selectStorage: (location, storage) => call("selectStorage", { location, storage }),
        calculateUsedSpace: (location) => call("calculateUsedSpace", { location }),
    };
}
const api = createTauriApi();

// ---- small helpers ----
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

function basename(path) {
    if (!path) {
        return "";
    }
    const idx = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    return idx >= 0 ? path.slice(idx + 1) : path;
}

function formatGiB(bytes) {
    const gib = Number(bytes || 0) / (1024 * 1024 * 1024);
    const digits = gib >= 100 ? 0 : (gib >= 10 ? 1 : 2);
    return gib.toFixed(digits) + " GiB";
}

function storageLabel(storage) {
    switch (Number(storage)) {
    case 1: return "eMMC";
    case 2: return "SD card";
    case 9: return "SPI NOR";
    default: return "storage";
    }
}

function storageOptionsFromMask(mask) {
    const out = [];
    if (mask & (1 << 0)) out.push({ value: 1, label: "eMMC" });
    if (mask & (1 << 1)) out.push({ value: 2, label: "SD card" });
    if (mask & (1 << 2)) out.push({ value: 9, label: "SPI NOR" });
    return out;
}

function showError(message) {
    runtimeError = message || "";
    errorStatus.textContent = runtimeError || dependencyWarning;
}

function setDependencyWarning(message) {
    dependencyWarning = message || "";
    errorStatus.textContent = runtimeError || dependencyWarning;
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

// ---- device list rendering ----
function deviceById(location) {
    return devices.find((d) => d.location === location);
}

function socLabel(d) {
    const soc = d.soc && d.soc !== "unknown" ? d.soc : "unknown SoC";
    return soc + " @ " + d.locationHex;
}

function opVerb(op) {
    switch (op) {
    case "connect": return "connecting";
    case "disconnect": return "disconnecting";
    case "reboot": return "rebooting";
    case "flash": return "flashing";
    case "erase": return "erasing";
    case "secure_erase": return "secure erasing";
    case "backup": return "backing up";
    default: return "working";
    }
}

// Which ops report real byte-progress (→ progress bar). The rest (connect/
// disconnect/erase) have no meaningful percentage, so they show animated dots.
function opHasProgress(op) {
    return op === "flash" || op === "secure_erase" || op === "backup";
}

// Animated ellipsis for no-progress ops: . → .. → ...
let dotTimer = null;
let dotCount = 1;

function anyDotOpsRunning() {
    return devices.some((d) => d.running && !opHasProgress(d.currentOp));
}

function ensureDotTimer() {
    if (anyDotOpsRunning() && !dotTimer) {
        dotTimer = setInterval(() => {
            dotCount = dotCount >= 3 ? 1 : dotCount + 1;
            paintDotStatuses();
        }, 400);
    } else if (!anyDotOpsRunning() && dotTimer) {
        clearInterval(dotTimer);
        dotTimer = null;
        dotCount = 1;
    }
}

function paintDotStatuses() {
    for (const d of devices) {
        if (d.running && !opHasProgress(d.currentOp)) {
            const card = deviceList.querySelector('.device-card[data-location="' + d.location + '"]');
            const st = card && card.querySelector(".dev-status");
            if (st) {
                st.textContent = opVerb(d.currentOp) + ".".repeat(dotCount);
            }
        }
    }
}

function statusTextFor(d) {
    if (!d.supported) {
        return "unsupported";
    }
    if (d.running) {
        if (opHasProgress(d.currentOp)) {
            const pct = d.progress >= 0 ? " " + d.progress + "%" : "";
            return opVerb(d.currentOp) + pct;
        }
        // No progress bar for this op — animated dots instead.
        return opVerb(d.currentOp) + ".".repeat(dotCount);
    }
    if (d.loaderReady) {
        return "connected";
    }
    // Present but not connected — show a neutral "detected" rather than the raw
    // USB mode (which reads "maskrom" even for a device with a loader).
    return "detected";
}

function dotColor(d) {
    if (!d.supported) return "#d9822b";
    if (d.running) return "#2a6fd9";
    if (d.loaderReady) return "#2fa84f";
    return "#2a6fd9";
}

// Hover tooltip for a device: the raw `rfi` flash info captured once at connect.
// (RAM/DDR size is not exposed by rkdeveloptool over USB, so it can't be shown
// here — it's only printed on the device's UART during DDR init.)
function deviceTooltip(d) {
    return (d.flashInfo || "").trim();
}

// Rebuild the whole list. Called on structural pushes (device added/removed,
// op start/stop, mode change) — never on progress ticks (those patch in place).
function renderDeviceList() {
    noDevices.style.display = devices.length === 0 ? "block" : "none";

    const wanted = new Set(devices.map((d) => String(d.location)));
    // Drop cards for devices that are gone.
    for (const card of Array.from(deviceList.children)) {
        if (!wanted.has(card.dataset.location)) {
            card.remove();
        }
    }

    for (const d of devices) {
        let card = deviceList.querySelector('.device-card[data-location="' + d.location + '"]');
        if (!card) {
            card = document.createElement("div");
            card.className = "device-card";
            card.dataset.location = String(d.location);
            deviceList.appendChild(card);
        }
        renderCard(card, d);
    }
    ensureDotTimer();
}

function renderCard(card, d) {
    const busy = d.running;
    const canConnect = d.supported && !busy && !d.loaderReady;
    const canOp = d.loaderReady && !busy;
    const storageOpts = storageOptionsFromMask(d.storageMask);

    card.innerHTML = `
        <div class="dev-head">
            <span class="dev-dot"></span>
            <span class="dev-title"></span>
            <span class="dev-status"></span>
        </div>
        <progress class="dev-progress" max="100" value="0"></progress>
        <div class="dev-controls"></div>
        <div class="dev-msg"></div>
        <div class="dev-result"></div>
    `;
    card.querySelector(".dev-dot").style.background = dotColor(d);
    // Show the app's connection state: once connected the device is running our
    // loader even if its USB descriptor still reads as maskrom (true on RK3588).
    const modeLabel = d.loaderReady ? "Loader" : d.mode;
    const title = card.querySelector(".dev-title");
    title.textContent = socLabel(d) + " (" + modeLabel + ")";
    // Hover tooltip: the flash info (rfi) captured once at connect. Prefix a
    // friendly storage size when we know it.
    title.title = deviceTooltip(d);
    title.style.cursor = d.flashInfo ? "help" : "default";
    card.querySelector(".dev-status").textContent = statusTextFor(d);

    // Progress bar only for ops that report a real percentage; connect (loader
    // download) and other no-progress ops show animated dots in the status text.
    const prog = card.querySelector(".dev-progress");
    const showBar = busy && opHasProgress(d.currentOp);
    prog.style.display = showBar ? "block" : "none";
    prog.value = d.progress >= 0 ? d.progress : 0;

    const controls = card.querySelector(".dev-controls");

    if (busy) {
        controls.appendChild(makeButton("Cancel", "cancel", d.location, false, "#a33"));
    } else if (!d.supported) {
        // nothing actionable
    } else if (!d.loaderReady) {
        controls.appendChild(makeButton("Connect", "connect", d.location, false, "#2a6fd9"));
    } else {
        // Connected: storage picker + operations.
        if (storageOpts.length > 0) {
            const sel = document.createElement("select");
            sel.className = "dev-storage";
            sel.dataset.location = String(d.location);
            for (const o of storageOpts) {
                const opt = document.createElement("option");
                opt.value = String(o.value);
                opt.textContent = o.label;
                if (o.value === d.selectedStorage) opt.selected = true;
                sel.appendChild(opt);
            }
            controls.appendChild(sel);
        }
        controls.appendChild(makeButton("Flash", "flash", d.location, !selectedImagePath, "#2a6fd9"));
        // Offered once a flash has succeeded on this device: reboot into the
        // freshly-flashed OS (rd 0), as opposed to Disconnect (rd 3 → maskrom).
        if (deviceFlashed[d.location]) {
            controls.appendChild(makeButton("Reboot", "reboot", d.location, false, "#2fa84f"));
        }
        controls.appendChild(makeButton("Backup", "backup", d.location, false));
        controls.appendChild(makeButton("Calc Used", "calculate", d.location, false));
        controls.appendChild(makeButton("Erase", "erase", d.location, false, "#a33"));
        controls.appendChild(makeButton("Secure Erase", "secure_erase", d.location, d.selectedStorage === 0, "#a33"));
        controls.appendChild(makeButton("Disconnect", "disconnect", d.location, false, "#a33"));
    }

    // Storage capacity / used space line (best-effort, cached).
    const msg = card.querySelector(".dev-msg");
    if (d.loaderReady && d.selectedStorage) {
        const cached = storageInfoCache[d.location + ":" + d.selectedStorage];
        msg.textContent = storageInfoLine(d.selectedStorage, cached);
        if (!cached && !busy) {
            fetchStorageInfo(d.location, d.selectedStorage);
        }
    } else {
        msg.textContent = "";
    }

    // Last-operation result message, shown in the card (not a popup). Hidden
    // (but NOT deleted) while an op runs: the completion event that sets a new
    // result can render before the not-busy device-list push arrives, and
    // deleting here would drop that just-set message (a failure would then look
    // silent). Every completion overwrites/clears deviceResults, so a stale
    // message from a previous op can't linger.
    const resultEl = card.querySelector(".dev-result");
    const result = deviceResults[d.location];
    if (result && !busy) {
        resultEl.textContent = result.text;
        resultEl.classList.toggle("err", !result.ok);
    } else {
        resultEl.textContent = "";
        resultEl.classList.remove("err");
    }
    void canConnect;
    void canOp;
}

function storageInfoLine(storage, info) {
    const parts = [storageLabel(storage) + ":"];
    if (!info || info.totalBytes == null) {
        parts.push("unknown size");
    } else {
        parts.push(formatGiB(info.totalBytes));
    }
    if (info && info.usedBytes != null) {
        parts.push("· Used: " + formatGiB(info.usedBytes));
    }
    return parts.join(" ");
}

async function fetchStorageInfo(location, storage) {
    if (!api || !api.getStorageInfo) {
        return;
    }
    // SD capacity via rfi is unreliable; the backend returns unknown for it.
    let total = null;
    try {
        const info = await api.getStorageInfo(location);
        if (info && info.success) {
            total = Number(pickField(info, "storageBytes", "storage_bytes") || 0) || null;
        }
    } catch (_) { /* best-effort */ }
    const key = location + ":" + storage;
    storageInfoCache[key] = Object.assign({ totalBytes: total, usedBytes: null }, storageInfoCache[key]);
    patchStorageLine(location);
}

function patchStorageLine(location) {
    const d = deviceById(location);
    if (!d) return;
    const card = deviceList.querySelector('.device-card[data-location="' + location + '"]');
    if (!card) return;
    const msg = card.querySelector(".dev-msg");
    if (msg && !msg.classList.contains("err") && d.loaderReady && d.selectedStorage) {
        msg.textContent = storageInfoLine(d.selectedStorage, storageInfoCache[location + ":" + d.selectedStorage]);
    }
}

function makeButton(label, act, location, disabled, bg) {
    const b = document.createElement("button");
    b.textContent = label;
    b.dataset.act = act;
    b.dataset.location = String(location);
    b.disabled = !!disabled;
    if (bg) {
        b.style.background = bg;
        b.style.color = "white";
    }
    return b;
}

// ---- host → UI device events ----
window.updateDeviceList = (list) => {
    devices = Array.isArray(list) ? list : [];
    // Forget per-device state for devices that are gone.
    const present = new Set(devices.map((d) => String(d.location)));
    for (const loc of Object.keys(deviceResults)) {
        if (!present.has(loc)) delete deviceResults[loc];
    }
    for (const loc of Object.keys(deviceFlashed)) {
        if (!present.has(loc)) delete deviceFlashed[loc];
    }
    for (const key of Object.keys(storageInfoCache)) {
        if (!present.has(key.split(":")[0])) delete storageInfoCache[key];
    }
    renderDeviceList();
};

// Frequent progress tick: patch one card without a rebuild.
window.onDeviceProgress = (location, percent) => {
    const card = deviceList.querySelector('.device-card[data-location="' + location + '"]');
    if (!card) return;
    const d = deviceById(location);
    // No-progress ops (connect/etc.) show dots, not a bar — ignore stray ticks.
    if (d && !opHasProgress(d.currentOp)) return;
    const value = Math.max(0, Math.min(100, Number(percent) || 0));
    const prog = card.querySelector(".dev-progress");
    if (prog) {
        prog.style.display = "block";
        prog.value = value;
    }
    if (d) {
        d.progress = value;
        const st = card.querySelector(".dev-status");
        if (st) st.textContent = statusTextFor(d);
    }
};

// Operation results are shown as a message inside the device's own card (via
// deviceResults), not as a modal popup. The host pushes a fresh device list
// right after this fires, which re-renders the card and picks up the message.
window.onDeviceOpComplete = (result) => {
    const location = result && result.location;
    const op = result && result.op;

    if (result && result.cancelled) {
        deviceResults[location] = { text: labelFor(op) + " canceled", ok: false };
        renderDeviceList();
        return;
    }
    if (!result || !result.success) {
        const err = (result && result.error) || "operation failed";
        deviceResults[location] = { text: labelFor(op) + " failed: " + err, ok: false };
        renderDeviceList();
        return;
    }

    // Success.
    if (op === "flash") {
        deviceFlashed[location] = true; // enable the Reboot button
    } else if (op === "reboot" || op === "disconnect") {
        delete deviceFlashed[location]; // device is leaving loader mode
    }
    const msg = successMessage(op);
    if (msg) {
        deviceResults[location] = { text: msg, ok: true };
    } else {
        delete deviceResults[location]; // e.g. connect — clear any stale message
    }
    renderDeviceList();
};

function labelFor(op) {
    switch (op) {
    case "connect": return "Connect";
    case "disconnect": return "Disconnect";
    case "reboot": return "Reboot";
    case "flash": return "Flash";
    case "erase": return "Quick Erase";
    case "secure_erase": return "Secure Erase";
    case "backup": return "Backup";
    default: return "Operation";
    }
}

function successMessage(op) {
    switch (op) {
    case "flash": return "Flash completed";
    case "erase": return "Quick Erase completed";
    case "secure_erase": return "Secure Erase completed (overwritten with zeros)";
    case "backup": return "Backup completed";
    case "reboot": return "Rebooting…";
    case "disconnect": return "Disconnected";
    case "connect": return ""; // connecting is silent; the green dot is enough
    default: return labelFor(op) + " completed";
    }
}

// ---- per-device control actions (event-delegated) ----
deviceList.addEventListener("click", async (event) => {
    const btn = event.target.closest("button[data-act]");
    if (!btn || btn.disabled) {
        return;
    }
    const location = Number(btn.dataset.location);
    const act = btn.dataset.act;
    const d = deviceById(location);
    if (!d || !api) {
        return;
    }
    switch (act) {
    case "connect":
        await runStart(() => api.flashBootloader(location), "connect");
        break;
    case "disconnect":
        await runStart(() => api.disconnectDevice(location), "disconnect");
        break;
    case "reboot":
        await runStart(() => api.rebootDevice(location), "reboot");
        break;
    case "flash":
        if (!selectedImagePath) {
            showError("Select a .img first");
            return;
        }
        await runStart(() => api.flashImage(location, selectedImagePath), "flash");
        break;
    case "erase": {
        const ok = await showConfirm(
            "Quick Erase on " + socLabel(d) + ": erases the partition table and OS, leaving the device " +
            "unbootable until reflashed. Not a guaranteed secure wipe. Continue?"
        );
        if (ok) await runStart(() => api.eraseStorage(location), "erase");
        break;
    }
    case "secure_erase": {
        const ok = await showConfirm(
            "Secure Erase on " + socLabel(d) + ": overwrites the entire " + storageLabel(d.selectedStorage) +
            " with zeros. Can take 15-60+ minutes and cannot be undone. Continue?"
        );
        if (ok) await runStart(() => api.secureEraseStorage(location), "secure erase");
        break;
    }
    case "backup":
        await startBackup(location);
        break;
    case "calculate":
        await calculateUsed(location);
        break;
    case "cancel": {
        const ok = await showConfirm(
            "Cancel the operation on " + socLabel(d) + "? This may leave the device in an unusable state."
        );
        if (ok) {
            try {
                await api.cancelFlash(location);
            } catch (e) {
                showError((e && e.message) || String(e) || "cancel failed");
            }
        }
        break;
    }
    default:
        break;
    }
});

// Per-device storage target change.
deviceList.addEventListener("change", async (event) => {
    const sel = event.target.closest("select.dev-storage");
    if (!sel || !api || !api.selectStorage) {
        return;
    }
    const location = Number(sel.dataset.location);
    const storage = Number(sel.value);
    try {
        const result = await api.selectStorage(location, storage);
        if (!result || result.started === false) {
            showError((result && result.error) || "storage selection failed");
            return;
        }
        const d = deviceById(location);
        if (d) d.selectedStorage = storage;
        // New target → invalidate cached used space; refresh capacity.
        delete storageInfoCache[location + ":" + storage];
        patchStorageLine(location);
        fetchStorageInfo(location, storage);
    } catch (e) {
        showError((e && e.message) || String(e) || "storage selection failed");
    }
});

async function runStart(invokeStart, label) {
    try {
        const result = await invokeStart();
        if (!result || result.started === false) {
            showError((result && result.error) || (label + " failed"));
            return false;
        }
        showError("");
        return true;
    } catch (e) {
        showError((e && e.message) || String(e) || (label + " failed"));
        return false;
    }
}

async function startBackup(location) {
    if (!api || !api.selectBackupDestination || !api.backupStorage) {
        showError("backup unavailable");
        return;
    }
    let picked;
    try {
        picked = await api.selectBackupDestination();
    } catch (e) {
        showError((e && e.message) || String(e) || "backup destination failed");
        return;
    }
    if (!picked || !picked.success) {
        return;
    }
    try {
        let result = await api.backupStorage(location, picked.path, false);
        if (result && result.started === false && pickField(result, "needsConfirmation", "needs_confirmation")) {
            const ok = await showConfirm(result.message);
            if (!ok) return;
            result = await api.backupStorage(location, picked.path, true);
        }
        if (!result || result.started === false) {
            showError((result && result.message) || "backup failed");
        } else {
            showError("");
        }
    } catch (e) {
        showError((e && e.message) || String(e) || "backup failed");
    }
}

async function calculateUsed(location) {
    if (!api || !api.calculateUsedSpace) {
        return;
    }
    const d = deviceById(location);
    const card = deviceList.querySelector('.device-card[data-location="' + location + '"]');
    const msg = card && card.querySelector(".dev-msg");
    if (msg) {
        msg.classList.remove("err");
        msg.textContent = storageLabel(d ? d.selectedStorage : 0) + ": calculating used space…";
    }
    try {
        const result = await api.calculateUsedSpace(location);
        if (!result || !result.success) {
            if (msg) { msg.classList.add("err"); msg.textContent = (result && result.error) || "calculate failed"; }
            return;
        }
        const used = Number(pickField(result, "usedBytes", "used_bytes") || 0);
        const storage = d ? d.selectedStorage : 0;
        const key = location + ":" + storage;
        storageInfoCache[key] = Object.assign({ totalBytes: null, usedBytes: used }, storageInfoCache[key]);
        storageInfoCache[key].usedBytes = used;
        if (msg) { msg.classList.remove("err"); }
        patchStorageLine(location);
    } catch (e) {
        if (msg) { msg.classList.add("err"); msg.textContent = (e && e.message) || "calculate failed"; }
    }
}

// ---- shared image selection + drag/drop ----
function applyImageSelection(path, sizeBytes) {
    selectedImagePath = path;
    selectedImageBytes = Number(sizeBytes || 0);
    selectedImage.textContent = basename(path) + " (" + formatGiB(selectedImageBytes) + ")";
    selectedImage.title = path + "\n" + selectedImageBytes.toLocaleString() + " bytes";
    showError("");
    renderDeviceList(); // enable Flash buttons
}

selectImage.addEventListener("click", async () => {
    if (!api || !api.selectImageFile) {
        showError("file picker unavailable");
        return;
    }
    const result = await api.selectImageFile();
    if (!result || !result.success) {
        return;
    }
    applyImageSelection(result.path, pickField(result, "sizeBytes", "size_bytes") || 0);
});

window.onImageFileDropped = (result) => {
    if (!result || !result.success || !result.path) {
        return;
    }
    applyImageSelection(result.path, pickField(result, "sizeBytes", "size_bytes") || 0);
};

window.onImageDragState = (state) => {
    const overlay = document.getElementById("dropOverlay");
    if (!overlay) return;
    const active = !!(state && state.active);
    const reject = active && !(state && state.valid);
    overlay.classList.toggle("active", active);
    overlay.classList.toggle("reject", reject);
    const text = document.getElementById("dropOverlayText");
    if (text) {
        text.textContent = reject ? "Drop a single .img file" : "Drop .img to select";
    }
};

window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => event.preventDefault());

// ---- log panel ----
const maxLiveLogLines = 5000;
let liveLogLineCount = 0;
let liveLogLastLineStart = 0;

function resetLiveLogTracking() {
    const value = liveLog.value;
    liveLogLineCount = 0;
    for (let i = 0; i < value.length; i += 1) {
        if (value[i] === "\n") liveLogLineCount += 1;
    }
    liveLogLastLineStart = value.length === 0 ? 0 : value.lastIndexOf("\n", value.length - 2) + 1;
}

function liveLogAtBottom() {
    return liveLog.scrollTop + liveLog.clientHeight >= liveLog.scrollHeight - 8;
}

window.appendLiveLog = (line, replaceLast) => {
    if (!line) return;
    const atBottom = liveLogAtBottom();
    if (replaceLast && liveLogLineCount > 0) {
        liveLog.value = liveLog.value.slice(0, liveLogLastLineStart) + line + "\n";
    } else {
        liveLogLastLineStart = liveLog.value.length;
        liveLog.value += line + "\n";
        liveLogLineCount += 1;
        if (liveLogLineCount > maxLiveLogLines) {
            const drop = Math.floor(maxLiveLogLines / 10);
            let cut = 0;
            for (let i = 0; i < drop; i += 1) {
                const next = liveLog.value.indexOf("\n", cut);
                if (next < 0) break;
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
        api.getLogContents().then((result) => {
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
        if (!api || !api.openLogDirectory) return;
        const result = await api.openLogDirectory();
        if (result && !result.success) {
            showError(result.error || "could not open log folder");
        }
    });
}

// ---- device access (driver / udev) ----
function setDriverInstallRunning(running) {
    driverInstallRunning = running;
    if (installDriver) installDriver.disabled = running;
    if (running) driverStatus.textContent = "Installing... (this may take a while)";
}

async function refreshDriverInfo() {
    if (!api || !api.getDeviceAccessInfo) return;
    try {
        const info = await api.getDeviceAccessInfo();
        if (!info || info.kind === "none") return;
        if (info.kind === "windows_driver") {
            if (!pickField(info, "deviceRelevant", "device_relevant")) {
                driverStatus.textContent = info.error || "device not found";
                return;
            }
            driverStatus.textContent = info.ready
                ? "Driver: " + (info.detail || "libusb-win32")
                : (info.error || ("Driver: " + (info.detail || "unknown")));
            return;
        }
        if (info.kind === "linux_udev") {
            driverStatus.textContent = info.ready
                ? "udev rules: installed"
                : (info.error || "udev rules: not installed — flashing may need root");
        }
    } catch (e) { /* best-effort */ }
}

async function initDriverInstallUi() {
    if (!installDriver) return;
    if (!api || !api.getDeviceAccessInfo || !api.installDeviceAccess) {
        installDriver.style.display = "none";
        driverStatus.style.display = "none";
        return;
    }
    let kind = "none";
    try {
        const info = await api.getDeviceAccessInfo();
        kind = (info && info.kind) || "none";
    } catch (e) {
        kind = "none";
    }
    if (kind === "none") {
        installDriver.style.display = "none";
        driverStatus.style.display = "none";
        return;
    }
    installDriver.textContent = kind === "linux_udev" ? "Install udev rules" : "Install libusb-win32";
    refreshDriverInfo();
    installDriver.addEventListener("click", async () => {
        if (driverInstallRunning) return;
        setDriverInstallRunning(true);
        if (kind === "linux_udev") {
            driverStatus.textContent = "Installing udev rules... (system authorization required)";
        }
        const result = await api.installDeviceAccess(driverDeviceName);
        if (!result || !result.started) {
            setDriverInstallRunning(false);
            driverStatus.textContent = (result && result.error) || "install already in progress";
        }
    });
}

window.onDriverInstallComplete = (result) => {
    setDriverInstallRunning(false);
    driverStatus.textContent = (!result || !result.success)
        ? ((result && result.error) || "install failed")
        : "installed";
    refreshDriverInfo();
};

async function refreshDependencyWarning() {
    if (!api || !api.getDependencyStatus) {
        setDependencyWarning("");
        return;
    }
    try {
        const status = await api.getDependencyStatus();
        setDependencyWarning(status && status.warning ? status.warning : "");
    } catch (e) {
        setDependencyWarning("Required dependency is missing - keep the application files together and reinstall if needed.");
    }
}

window.onQuitDuringOperation = async () => {
    if (quitPromptShowing) return;
    quitPromptShowing = true;
    try {
        const ok = await showConfirm(
            "A flash, erase, or backup is still in progress. Quitting now may leave a device " +
            "partially written or needing to be reflashed. Quit anyway?"
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
        initDriverInstallUi();
        if (api && api.uiReady) {
            api.uiReady();
        }
    }, 0);
});

renderDeviceList();
