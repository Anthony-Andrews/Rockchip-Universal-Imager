//! Rockchip VID/PID → SoC name and optional bundled SPL loader filename.
//!
//! Filenames refer to `loader_binaries/`, built from the official rkbin repository

#[derive(Clone, Copy)]
pub struct LoaderMapEntry {
    pub vid: u16,
    pub pid: u16,
    pub soc: &'static str,
    pub filename: Option<&'static str>,
}

const LOADER_MAP: &[LoaderMapEntry] = &[
    LoaderMapEntry { vid: 0x2207, pid: 0x110a, soc: "RV1108", filename: Some("rv110x_loader_v1.12.126.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x110b, soc: "RV1126", filename: Some("rv1126_spl_loader_v1.16.110.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x110c, soc: "RV1106", filename: Some("rv1106_download_v1.15.108.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x110e, soc: "RV1103B", filename: Some("rv1103b_download_v1.06.100.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x110f, soc: "RV1126B", filename: Some("rv1126b_spl_loader_v1.09.105.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x180a, soc: "RK1808", filename: Some("rk1808_loader_v1.06.109.bin") },
    // RK2818/RK2918/RK2928 predate rkbin - no loader blobs exist in it, at any revision.
    LoaderMapEntry { vid: 0x2207, pid: 0x281a, soc: "RK2818", filename: None },
    LoaderMapEntry { vid: 0x2207, pid: 0x290a, soc: "RK2918", filename: None },
    LoaderMapEntry { vid: 0x2207, pid: 0x292a, soc: "RK2928", filename: None },
    LoaderMapEntry { vid: 0x2207, pid: 0x292c, soc: "RK3026", filename: Some("RK3026Loader_miniall.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x300a, soc: "RK3066", filename: Some("RK3066Loader_miniall.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x300b, soc: "RK3168", filename: Some("RK3168Loader_miniall.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x301a, soc: "RK3036", filename: Some("rk3036_loader_v1.11.257.bin") },
    // RK3066B has no recipe of its own; the RK3188-family loader is a plausible
    // match but is unverified on hardware, so it is left unsupported for now.
    LoaderMapEntry { vid: 0x2207, pid: 0x310a, soc: "RK3066B", filename: None },
    LoaderMapEntry { vid: 0x2207, pid: 0x310b, soc: "RK3188", filename: Some("rk3188_loader_v2.00.200.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x310c, soc: "RK3126/RK3128", filename: Some("rk3128_loader_v2.12.263.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x310d, soc: "RK3126", filename: Some("rk3126_loader_v2.09.263.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x320a, soc: "RK3288", filename: Some("rk3288_loader_v1.12.263.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x320b, soc: "RK3228/RK3229", filename: Some("rk322x_loader_v1.10.256.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x320c, soc: "RK3328", filename: Some("rk3328_loader_v1.22.250.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x330a, soc: "RK3368", filename: Some("rk3368_loader_v2.06.268.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x330c, soc: "RK3399/OP1", filename: Some("rk3399_loader_v1.30.130.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x330d, soc: "PX30", filename: Some("px30_loader_v2.12.135.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x330e, soc: "RK3308", filename: Some("rk3308_loader_v2.11.143.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350a, soc: "RK3566/RK3568", filename: Some("rk356x_spl_loader_v1.25.114.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350b, soc: "RK3588/RK3582", filename: Some("rk3588_spl_loader_v1.21.114.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350c, soc: "RK3528", filename: Some("rk3528_spl_loader_v1.13.106.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350d, soc: "RK3562", filename: Some("rk3562_spl_loader_v1.09.107.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350e, soc: "RK3576", filename: Some("rk3576_spl_loader_v1.12.108.bin") },
    LoaderMapEntry { vid: 0x2207, pid: 0x350f, soc: "RK3506", filename: Some("rk3506_spl_loader_v1.06.112.bin") },
];

pub fn entry_for(vid: u16, pid: u16) -> Option<&'static LoaderMapEntry> {
    LOADER_MAP.iter().find(|e| e.vid == vid && e.pid == pid)
}
