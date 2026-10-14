//! Displays: Windows display settings (mode, HDR, scaling, rotation) and monitor
//! hardware controls over DDC/CI (brightness, contrast, colour preset, input, volume).
//!
//! These are live controls; Windows and the monitor remember them on their own.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Devices::Display::*,
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::*,
    },
    core::{BOOL, PCWSTR},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} × {} @ {} Hz", self.width, self.height, self.hz)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// GDI name, e.g. `\\.\DISPLAY1` (the handle for changes).
    pub device: String,
    /// Monitor name from EDID, e.g. "PL4373UH".
    pub name: String,
    /// EDID manufacturer (PnP id), e.g. "IVM".
    pub vendor: String,
    /// Monitor device key (registry path), used for colour profiles.
    pub monitor_key: Option<String>,
    pub primary: bool,
    pub mode: Mode,
    /// 0, 90, 180, 270.
    pub rotation: u32,
    /// Every mode the monitor offers at 32-bit colour, largest first.
    pub modes: Vec<Mode>,
    pub hdr_supported: bool,
    pub hdr_on: bool,
    /// Bit depth, colour encoding and SDR content brightness of the link.
    pub hdr_info: Option<crate::graphics::HdrInfo>,
    /// Current scaling in percent and the choices Windows allows.
    pub scale: Option<u32>,
    pub scales: Vec<u32>,
    pub ddc: Option<Ddc>,
}

// ---------------------------------------------------------------------------
// GDI: monitors, modes

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn from_wide(w: &[u16]) -> String {
    String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())])
}

fn hmonitors() -> Vec<HMONITOR> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: data is the &mut Vec passed below, alive for the enumeration.
        unsafe { (*(data.0 as *mut Vec<HMONITOR>)).push(h) };
        true.into()
    }
    let mut out: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only touches `out`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

fn monitor_device(h: HMONITOR) -> Option<(String, bool)> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: cbSize set for the EX variant.
    unsafe { GetMonitorInfoW(h, &mut info as *mut _ as *mut MONITORINFO) }
        .as_bool()
        .then(|| {
            (
                from_wide(&info.szDevice),
                info.monitorInfo.dwFlags & 1 != 0, /* MONITORINFOF_PRIMARY */
            )
        })
}

fn monitor_key(device: &str) -> Option<String> {
    let name = wide(device);
    let mut dd = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    // SAFETY: cb set; first monitor attached to this adapter output.
    unsafe { EnumDisplayDevicesW(PCWSTR(name.as_ptr()), 0, &mut dd, 0) }
        .as_bool()
        .then(|| from_wide(&dd.DeviceKey))
        .filter(|k| !k.is_empty())
}

fn devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn current(device: &str) -> Option<(Mode, u32)> {
    let name = wide(device);
    let mut dm = devmode();
    // SAFETY: dm sized; name NUL-terminated.
    unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) }
        .as_bool()
        .then(|| {
            // SAFETY: display devices fill the display variant of the union.
            let o = unsafe { dm.Anonymous1.Anonymous2.dmDisplayOrientation };
            (
                Mode {
                    width: dm.dmPelsWidth,
                    height: dm.dmPelsHeight,
                    hz: dm.dmDisplayFrequency,
                },
                o.0 * 90,
            )
        })
}

fn modes(device: &str) -> Vec<Mode> {
    let name = wide(device);
    let mut out = Vec::new();
    for i in 0.. {
        let mut dm = devmode();
        // SAFETY: as above.
        if !unsafe {
            EnumDisplaySettingsW(
                PCWSTR(name.as_ptr()),
                ENUM_DISPLAY_SETTINGS_MODE(i),
                &mut dm,
            )
        }
        .as_bool()
        {
            break;
        }
        if dm.dmBitsPerPel == 32 && dm.dmDisplayFrequency > 1 {
            out.push(Mode {
                width: dm.dmPelsWidth,
                height: dm.dmPelsHeight,
                hz: dm.dmDisplayFrequency,
            });
        }
    }
    out.sort_by(|a, b| b.cmp(a));
    out.dedup();
    out
}

/// Changes resolution/refresh (and optionally rotation) and saves it.
pub fn set_mode(device: &str, mode: Mode, rotation: Option<u32>) -> Result<()> {
    let name = wide(device);
    let mut dm = devmode();
    dm.dmPelsWidth = mode.width;
    dm.dmPelsHeight = mode.height;
    dm.dmDisplayFrequency = mode.hz;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    if let Some(r) = rotation {
        if r % 90 != 0 || r > 270 {
            bail!("rotation must be 0, 90, 180 or 270");
        }
        let (_, cur_rot) = current(device).context("reading current mode")?;
        // Portrait <-> landscape swaps the axes.
        if (r / 90) % 2 != (cur_rot / 90) % 2 {
            std::mem::swap(&mut dm.dmPelsWidth, &mut dm.dmPelsHeight);
        }
        dm.Anonymous1.Anonymous2.dmDisplayOrientation = DEVMODE_DISPLAY_ORIENTATION(r / 90);
        dm.dmFields |= DM_DISPLAYORIENTATION;
    }
    // SAFETY: dm fully initialised; name NUL-terminated.
    let r = unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(name.as_ptr()),
            Some(&dm),
            None,
            CDS_UPDATEREGISTRY,
            None,
        )
    };
    if r != DISP_CHANGE_SUCCESSFUL {
        bail!("display mode change failed ({})", r.0);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DisplayConfig: names, HDR, scaling

struct Path {
    source: DISPLAYCONFIG_PATH_SOURCE_INFO,
    target: DISPLAYCONFIG_PATH_TARGET_INFO,
}

fn paths() -> Result<Vec<Path>> {
    let (mut np, mut nm) = (0u32, 0u32);
    // SAFETY: standard two-call buffer pattern.
    unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).ok()?;
        let mut p = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut m = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            p.as_mut_ptr(),
            &mut nm,
            m.as_mut_ptr(),
            None,
        )
        .ok()?;
        p.truncate(np as usize);
        Ok(p.into_iter()
            .map(|x| Path {
                source: x.sourceInfo,
                target: x.targetInfo,
            })
            .collect())
    }
}

fn header<T>(
    kind: i32,
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(kind),
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    }
}

fn source_name(p: &Path) -> Option<String> {
    let mut v = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME.0,
            p.source.adapterId,
            p.source.id,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches the struct.
    (unsafe { DisplayConfigGetDeviceInfo(&mut v.header) } == 0)
        .then(|| from_wide(&v.viewGdiDeviceName))
}

fn target_vendor(p: &Path) -> Option<String> {
    let mut v = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME.0,
            p.target.adapterId,
            p.target.id,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches the struct.
    (unsafe { DisplayConfigGetDeviceInfo(&mut v.header) } == 0 && v.edidManufactureId != 0)
        .then(|| crate::icc::pnp_id(v.edidManufactureId))
}

fn target_name(p: &Path) -> Option<String> {
    let mut v = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME.0,
            p.target.adapterId,
            p.target.id,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches the struct.
    (unsafe { DisplayConfigGetDeviceInfo(&mut v.header) } == 0)
        .then(|| from_wide(&v.monitorFriendlyDeviceName))
        .filter(|n| !n.is_empty())
}

/// (supported, enabled)
fn hdr(p: &Path) -> (bool, bool) {
    let mut v = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO.0,
            p.target.adapterId,
            p.target.id,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches; the union is read as its raw bit field.
    unsafe {
        if DisplayConfigGetDeviceInfo(&mut v.header) != 0 {
            return (false, false);
        }
        let bits = v.Anonymous.value;
        // bit 0: advancedColorSupported, bit 1: advancedColorEnabled
        (bits & 1 != 0, bits & 2 != 0)
    }
}

fn set_hdr_on(p: &Path, on: bool) -> Result<()> {
    let mut v = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
        header: header::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(
            DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE.0,
            p.target.adapterId,
            p.target.id,
        ),
        ..Default::default()
    };
    v.Anonymous.value = on as u32;
    // SAFETY: header size matches the struct.
    let r = unsafe { DisplayConfigSetDeviceInfo(&v.header) };
    if r != 0 {
        bail!("switching HDR failed ({r})");
    }
    Ok(())
}

// Scaling: not in the public headers, but it's how Settings does it and the
// layout has been stable since Windows 10 1607 (see e.g. the open-source SetDpi
// tool). Values are relative steps from the recommended scale.
const DPI_GET: i32 = -3;
const DPI_SET: i32 = -4;
const DPI_STEPS: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

#[repr(C)]
struct DpiGet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min_rel: i32,
    cur_rel: i32,
    max_rel: i32,
}

#[repr(C)]
struct DpiSet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    rel: i32,
}

/// (current %, allowed %s, recommended index)
fn scaling(p: &Path) -> Option<(u32, Vec<u32>, usize)> {
    let mut v = DpiGet {
        header: header::<DpiGet>(DPI_GET, p.source.adapterId, p.source.id),
        min_rel: 0,
        cur_rel: 0,
        max_rel: 0,
    };
    // SAFETY: the header's size field describes DpiGet.
    if unsafe { DisplayConfigGetDeviceInfo(&mut v.header) } != 0 {
        return None;
    }
    let rec = v.min_rel.unsigned_abs() as usize;
    let cur = (rec as i32 + v.cur_rel).clamp(0, DPI_STEPS.len() as i32 - 1) as usize;
    let last = (rec as i32 + v.max_rel).clamp(0, DPI_STEPS.len() as i32 - 1) as usize;
    Some((DPI_STEPS[cur], DPI_STEPS[..=last].to_vec(), rec))
}

fn set_scaling(p: &Path, percent: u32) -> Result<()> {
    let (_, allowed, rec) = scaling(p).context("scaling not available on this display")?;
    let idx = allowed
        .iter()
        .position(|&s| s == percent)
        .with_context(|| format!("{percent}% is not offered"))?;
    let v = DpiSet {
        header: header::<DpiSet>(DPI_SET, p.source.adapterId, p.source.id),
        rel: idx as i32 - rec as i32,
    };
    // SAFETY: the header's size field describes DpiSet.
    let r = unsafe { DisplayConfigSetDeviceInfo(&v.header) };
    if r != 0 {
        bail!("changing scaling failed ({r})");
    }
    Ok(())
}

fn path_for(device: &str) -> Result<Path> {
    paths()?
        .into_iter()
        .find(|p| source_name(p).as_deref() == Some(device))
        .with_context(|| format!("no active display {device}"))
}

pub fn set_hdr(device: &str, on: bool) -> Result<()> {
    set_hdr_on(&path_for(device)?, on)
}

/// SDR content brightness while HDR is on (80-480 nits).
pub fn set_sdr_brightness(device: &str, nits: u32) -> Result<()> {
    let p = path_for(device)?;
    crate::graphics::set_sdr_nits(p.target.adapterId, p.target.id, nits)
}

pub fn set_scale(device: &str, percent: u32) -> Result<()> {
    set_scaling(&path_for(device)?, percent)
}

// ---------------------------------------------------------------------------
// DDC/CI

pub mod vcp {
    pub const BRIGHTNESS: u8 = 0x10;
    pub const CONTRAST: u8 = 0x12;
    pub const COLOR_PRESET: u8 = 0x14;
    pub const RED_GAIN: u8 = 0x16;
    pub const GREEN_GAIN: u8 = 0x18;
    pub const BLUE_GAIN: u8 = 0x1a;
    pub const INPUT: u8 = 0x60;
    pub const VOLUME: u8 = 0x62;
    pub const SHARPNESS: u8 = 0x87;
    pub const MUTE: u8 = 0x8d;
    pub const POWER: u8 = 0xd6;
    pub const RESET_ALL: u8 = 0x04;
    pub const RESET_BRIGHTNESS_CONTRAST: u8 = 0x05;
    pub const RESET_COLOR: u8 = 0x08;
    pub const COLOR_TEMP_INCREMENT: u8 = 0x0b;
    pub const COLOR_TEMP: u8 = 0x0c;
    pub const OSD: u8 = 0xca;
    pub const OSD_LANGUAGE: u8 = 0xcc;

    /// MCCS: OSD disabled / enabled (VCP 0xCA).
    pub const OSD_LOCKED: u32 = 1;
    pub const OSD_UNLOCKED: u32 = 2;
}

/// MCCS 2.2 name of a VCP code (for the scanner).
pub fn vcp_name(code: u8) -> &'static str {
    match code {
        0x01 => "Degauss",
        0x02 => "New control value",
        0x03 => "Soft controls",
        0x04 => "Restore factory defaults",
        0x05 => "Restore brightness/contrast",
        0x06 => "Restore geometry",
        0x08 => "Restore colour",
        0x0b => "Colour temperature increment",
        0x0c => "Colour temperature",
        0x0e => "Clock",
        0x10 => "Brightness",
        0x11 => "Flesh tone enhancement",
        0x12 => "Contrast",
        0x13 => "Backlight control",
        0x14 => "Colour preset",
        0x16 => "Red gain",
        0x17 => "User colour vision compensation",
        0x18 => "Green gain",
        0x1a => "Blue gain",
        0x1e => "Auto setup",
        0x1f => "Auto colour setup",
        0x20 => "Horizontal position",
        0x30 => "Vertical position",
        0x3e => "Clock phase",
        0x52 => "Active control",
        0x56 => "Horizontal moiré",
        0x58 => "Vertical moiré",
        0x59..=0x5e => "Six-axis saturation",
        0x60 => "Input source",
        0x62 => "Speaker volume",
        0x63 => "Speaker select",
        0x6c => "Red black level",
        0x6e => "Green black level",
        0x70 => "Blue black level",
        0x72 => "Gamma",
        0x86 => "Display scaling",
        0x87 => "Sharpness",
        0x8a => "Saturation",
        0x8d => "Audio mute",
        0x90 => "Hue",
        0x9b..=0xa0 => "Six-axis hue",
        0xa2 => "Auto setup on/off",
        0xaa => "Screen orientation",
        0xac => "Horizontal frequency",
        0xae => "Vertical frequency",
        0xb2 => "Flat panel sub-pixel layout",
        0xb6 => "Display technology",
        0xc0 => "Display usage time",
        0xc6 => "Application enable key",
        0xc8 => "Display controller type",
        0xc9 => "Firmware level",
        0xca => "OSD / button control",
        0xcc => "OSD language",
        0xd6 => "Power mode",
        0xdc => "Display mode",
        0xdf => "VCP version",
        0xe0..=0xff => "Manufacturer specific",
        _ => "",
    }
}

/// MCCS OSD language codes (VCP 0xCC).
pub fn language_name(v: u8) -> String {
    const NAMES: [&str; 0x26] = [
        "Reserved",
        "Chinese (traditional)",
        "English",
        "French",
        "German",
        "Italian",
        "Japanese",
        "Korean",
        "Portuguese (Portugal)",
        "Russian",
        "Spanish",
        "Swedish",
        "Turkish",
        "Chinese (simplified)",
        "Portuguese (Brazil)",
        "Arabic",
        "Bulgarian",
        "Croatian",
        "Czech",
        "Danish",
        "Dutch",
        "Estonian",
        "Finnish",
        "Greek",
        "Hebrew",
        "Hindi",
        "Hungarian",
        "Latvian",
        "Lithuanian",
        "Norwegian",
        "Polish",
        "Romanian",
        "Serbian",
        "Slovak",
        "Slovenian",
        "Thai",
        "Ukrainian",
        "Vietnamese",
    ];
    NAMES
        .get(v as usize)
        .map_or_else(|| format!("Language {v:#04x}"), |n| n.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct VcpInfo {
    pub code: u8,
    /// Momentary (an action) vs set parameter (a value).
    pub momentary: bool,
    pub current: u32,
    pub max: u32,
    /// Listed in the monitor's capability string.
    pub listed: bool,
}

impl Ddc {
    /// Colour temperature in Kelvin: (current, min, max, step).
    pub fn color_temp(&self) -> Option<(u32, u32, u32, u32)> {
        let (cur, max) = *self.values.get(&vcp::COLOR_TEMP)?;
        let step = self
            .get(vcp::COLOR_TEMP_INCREMENT)
            .filter(|s| (1..=1000).contains(s))
            .unwrap_or(100);
        Some((3000 + cur * step, 3000, 3000 + max * step, step))
    }
}

pub fn control_name(code: u8) -> String {
    match code {
        vcp::BRIGHTNESS => "brightness".into(),
        vcp::CONTRAST => "contrast".into(),
        vcp::COLOR_PRESET => "the colour preset".into(),
        vcp::RED_GAIN => "red gain".into(),
        vcp::GREEN_GAIN => "green gain".into(),
        vcp::BLUE_GAIN => "blue gain".into(),
        vcp::VOLUME => "volume".into(),
        vcp::SHARPNESS => "sharpness".into(),
        c => format!("VCP {c:#04x}"),
    }
}

/// MCCS 2.2 names for input source values (VCP 0x60).
pub fn input_name(v: u8) -> String {
    match v {
        0x01 => "VGA 1".into(),
        0x02 => "VGA 2".into(),
        0x03 => "DVI 1".into(),
        0x04 => "DVI 2".into(),
        0x0f => "DisplayPort 1".into(),
        0x10 => "DisplayPort 2".into(),
        0x11 => "HDMI 1".into(),
        0x12 => "HDMI 2".into(),
        0x13 => "HDMI 3".into(),
        0x14 => "HDMI 4".into(),
        0x1b => "USB-C".into(),
        v => format!("Input {v:#04x}"),
    }
}

/// MCCS 2.2 names for colour preset values (VCP 0x14).
pub fn preset_name(v: u8) -> String {
    match v {
        0x01 => "sRGB".into(),
        0x02 => "Native".into(),
        0x03 => "4000 K".into(),
        0x04 => "5000 K".into(),
        0x05 => "6500 K".into(),
        0x06 => "7500 K".into(),
        0x07 => "8200 K".into(),
        0x08 => "9300 K".into(),
        0x09 => "10000 K".into(),
        0x0a => "11500 K".into(),
        0x0b => "User 1".into(),
        0x0c => "User 2".into(),
        0x0d => "User 3".into(),
        v => format!("Preset {v:#04x}"),
    }
}

/// Supported VCP codes and, where listed, their allowed values.
pub fn parse_caps(caps: &str) -> BTreeMap<u8, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Some(start) = caps.find("vcp(") else {
        return out;
    };
    let body = &caps[start + 4..];
    let mut depth = 0usize;
    let mut code: Option<u8> = None;
    let mut token = String::new();
    let flush = |token: &mut String,
                 depth: usize,
                 code: &mut Option<u8>,
                 out: &mut BTreeMap<u8, Vec<u8>>| {
        if let Ok(v) = u8::from_str_radix(token, 16) {
            if depth == 0 {
                *code = Some(v);
                out.entry(v).or_default();
            } else if let Some(c) = code {
                out.entry(*c).or_default().push(v);
            }
        }
        token.clear();
    };
    for ch in body.chars() {
        match ch {
            '(' => {
                flush(&mut token, depth, &mut code, &mut out);
                depth += 1;
            }
            ')' => {
                flush(&mut token, depth, &mut code, &mut out);
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            c if c.is_ascii_whitespace() => flush(&mut token, depth, &mut code, &mut out),
            c => token.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ddc {
    /// Model from the capability string (`model(...)`), often the retail name.
    pub model: Option<String>,
    /// Supported codes -> allowed values (empty = continuous).
    pub caps: BTreeMap<u8, Vec<u8>>,
    /// Current (value, maximum) of the continuous controls we show.
    pub values: BTreeMap<u8, (u32, u32)>,
}

impl Ddc {
    pub fn supports(&self, code: u8) -> bool {
        self.caps.contains_key(&code)
    }

    pub fn get(&self, code: u8) -> Option<u32> {
        self.values.get(&code).map(|v| v.0)
    }
}

struct Physical(Vec<PHYSICAL_MONITOR>);

impl Drop for Physical {
    fn drop(&mut self) {
        // SAFETY: handles came from GetPhysicalMonitorsFromHMONITOR.
        let _ = unsafe { DestroyPhysicalMonitors(&self.0) };
    }
}

fn physical(h: HMONITOR) -> Option<Physical> {
    let mut n = 0u32;
    // SAFETY: count query then fill a buffer of that size.
    unsafe {
        GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n).ok()?;
        let mut v = vec![PHYSICAL_MONITOR::default(); n as usize];
        GetPhysicalMonitorsFromHMONITOR(h, &mut v).ok()?;
        (!v.is_empty()).then_some(Physical(v))
    }
}

fn hmonitor_for(device: &str) -> Option<HMONITOR> {
    hmonitors()
        .into_iter()
        .find(|&h| monitor_device(h).is_some_and(|(d, _)| d == device))
}

fn read_vcp(m: &PHYSICAL_MONITOR, code: u8) -> Option<(u32, u32)> {
    // Monitors occasionally drop a reply; one retry is usually enough.
    for _ in 0..2 {
        let (mut cur, mut max) = (0u32, 0u32);
        // SAFETY: plain out values.
        if unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                m.hPhysicalMonitor,
                code,
                None,
                &mut cur,
                Some(&mut max),
            )
        } != 0
        {
            return Some((cur, max));
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    None
}

fn read_caps(m: &PHYSICAL_MONITOR) -> Option<String> {
    let mut len = 0u32;
    // SAFETY: length query then a buffer of that size.
    unsafe {
        if GetCapabilitiesStringLength(m.hPhysicalMonitor, &mut len) == 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if CapabilitiesRequestAndCapabilitiesReply(m.hPhysicalMonitor, &mut buf) == 0 {
            return None;
        }
        Some(
            String::from_utf8_lossy(&buf)
                .trim_end_matches('\0')
                .to_string(),
        )
    }
}

/// Codes whose current value we read (and show).
const SHOWN: &[u8] = &[
    vcp::COLOR_TEMP_INCREMENT,
    vcp::COLOR_TEMP,
    vcp::OSD_LANGUAGE,
    vcp::BRIGHTNESS,
    vcp::CONTRAST,
    vcp::COLOR_PRESET,
    vcp::RED_GAIN,
    vcp::GREEN_GAIN,
    vcp::BLUE_GAIN,
    vcp::INPUT,
    vcp::VOLUME,
    vcp::SHARPNESS,
    vcp::MUTE,
];

thread_local! {
    /// Capability strings are slow (~1 s) and never change: cache per device+name.
    static CAPS: std::cell::RefCell<BTreeMap<String, Option<String>>> = Default::default();
}

fn ddc_for(h: HMONITOR, key: &str) -> Option<Ddc> {
    let phys = physical(h)?;
    let m = phys.0.first()?;
    let raw = CAPS.with(|c| {
        c.borrow_mut()
            .entry(key.to_string())
            .or_insert_with(|| read_caps(m))
            .clone()
    })?;
    let caps = parse_caps(&raw);
    let model = raw
        .split("model(")
        .nth(1)
        .and_then(|r| r.split(')').next())
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let values = SHOWN
        .iter()
        .filter(|c| caps.contains_key(c))
        .filter_map(|&c| read_vcp(m, c).map(|(cur, max)| (c, (to_menu(m, c, cur, max), max))))
        .collect();
    Some(Ddc {
        model,
        caps,
        values,
    })
}

// Some monitors report the *effective* value over DDC/CI (e.g. backlight after an
// ECO cap) while their menu shows the setting. We learn the factor from our own
// writes (wrote 50, monitor reports 28 -> 0.56) and show values in menu terms.

fn scale_file() -> Option<std::path::PathBuf> {
    Some(
        std::path::PathBuf::from(std::env::var_os("APPDATA")?)
            .join("HIDConfig")
            .join("ddc-scale.toml"),
    )
}

static SCALES: std::sync::Mutex<Option<BTreeMap<String, f64>>> = std::sync::Mutex::new(None);

fn with_scales<R>(f: impl FnOnce(&mut BTreeMap<String, f64>) -> R) -> R {
    let mut guard = SCALES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(|| {
        scale_file()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    });
    f(map)
}

fn scale_key(m: &PHYSICAL_MONITOR, code: u8) -> String {
    // Packed struct: copy the field before borrowing it.
    let desc = m.szPhysicalMonitorDescription;
    format!("{} {code:02x}", from_wide(&desc))
}

fn learn_scale(m: &PHYSICAL_MONITOR, code: u8, written: u32, reported: u32) {
    let key = scale_key(m, code);
    let ratio = reported as f64 / written as f64;
    with_scales(|map| {
        if written >= 10 && (0.1..0.97).contains(&ratio) {
            map.insert(key, ratio);
        } else if reported == written {
            map.remove(&key);
        } else {
            return;
        }
        if let Some(p) = scale_file() {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, toml::to_string(&*map).unwrap_or_default());
        }
    });
}

/// Converts a reported value to the monitor's menu scale, if a factor is known.
fn to_menu(m: &PHYSICAL_MONITOR, code: u8, reported: u32, max: u32) -> u32 {
    with_scales(|map| map.get(&scale_key(m, code)).copied()).map_or(reported, |r| {
        ((reported as f64 / r).round() as u32).min(max)
    })
}

/// Sets a VCP value on the monitor shown as `device`.
pub fn set_vcp(device: &str, code: u8, value: u32) -> Result<()> {
    let h = hmonitor_for(device).with_context(|| format!("no display {device}"))?;
    let phys = physical(h).context("monitor doesn't expose a physical handle")?;
    let m = phys.0.first().context("no physical monitor")?;
    let before = read_vcp(m, code);
    // SAFETY: valid physical monitor handle.
    if unsafe { SetVCPFeature(m.hPhysicalMonitor, code, value) } == 0 {
        bail!("monitor didn't accept VCP {code:#04x} = {value} (is DDC/CI enabled in its menu?)");
    }
    // Some monitors acknowledge the write but ignore it while a picture mode owns
    // the setting (e.g. dynamic contrast / ECO locking brightness). Say so.
    let Some((before, max)) = before else {
        return Ok(());
    };
    let not_a_level = [
        vcp::INPUT,
        vcp::COLOR_PRESET,
        vcp::RESET_ALL,
        vcp::RESET_BRIGHTNESS_CONTRAST,
        vcp::RESET_COLOR,
        vcp::OSD,
        vcp::OSD_LANGUAGE,
        vcp::POWER,
    ];
    if not_a_level.contains(&code) || to_menu(m, code, before, max) == value {
        return Ok(());
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let Some((after, _)) = read_vcp(m, code) else {
        return Ok(());
    };
    if after != before {
        learn_scale(m, code, value, after);
        return Ok(());
    }
    let ratio = with_scales(|map| map.get(&scale_key(m, code)).copied()).unwrap_or(1.0);
    let expected = (value as f64 * ratio).round() as i64;
    if (expected - before as i64).abs() >= 2 {
        bail!(
            "the monitor kept {} at {} - it's probably locked by a picture mode (dynamic/advanced contrast, ECO, a game preset); turn that off in the monitor's menu",
            control_name(code),
            to_menu(m, code, before, max)
        );
    }
    Ok(())
}

/// Sets the colour temperature in Kelvin (rounded to the monitor's step).
pub fn set_color_temp(device: &str, kelvin: u32) -> Result<()> {
    let h = hmonitor_for(device).with_context(|| format!("no display {device}"))?;
    let phys = physical(h).context("monitor doesn't expose a physical handle")?;
    let m = phys.0.first().context("no physical monitor")?;
    let step = read_vcp(m, vcp::COLOR_TEMP_INCREMENT)
        .map(|v| v.0)
        .filter(|s| (1..=1000).contains(s))
        .unwrap_or(100);
    let max = read_vcp(m, vcp::COLOR_TEMP)
        .map(|v| v.1)
        .context("monitor has no colour temperature control")?;
    let value = (kelvin.saturating_sub(3000) + step / 2) / step;
    drop(phys);
    set_vcp(device, vcp::COLOR_TEMP, value.min(max))
}

/// Reads every VCP code 0x00-0xFF (read-only; ~10-20 s). Works on any DDC/CI monitor.
pub fn scan(device: &str) -> Result<Vec<VcpInfo>> {
    let h = hmonitor_for(device).with_context(|| format!("no display {device}"))?;
    let phys = physical(h).context("monitor doesn't expose a physical handle")?;
    let m = phys.0.first().context("no physical monitor")?;
    let listed = read_caps(m).map(|c| parse_caps(&c)).unwrap_or_default();
    let mut out = Vec::new();
    for code in 0..=255u8 {
        for _ in 0..2 {
            let (mut kind, mut cur, mut max) = (MC_VCP_CODE_TYPE::default(), 0u32, 0u32);
            // SAFETY: plain out values on a valid handle.
            let ok = unsafe {
                GetVCPFeatureAndVCPFeatureReply(
                    m.hPhysicalMonitor,
                    code,
                    Some(&mut kind),
                    &mut cur,
                    Some(&mut max),
                )
            } != 0;
            if ok {
                out.push(VcpInfo {
                    code,
                    momentary: kind == MC_MOMENTARY,
                    current: cur,
                    max,
                    listed: listed.contains_key(&code),
                });
                break;
            }
            // Advertised codes get one retry; the rest are probed once.
            if !listed.contains_key(&code) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------

/// All active displays. `with_ddc` also queries the monitors (slow: ~0.1-1 s each).
pub fn displays(with_ddc: bool) -> Result<Vec<Display>> {
    let paths = paths().unwrap_or_default();
    let mut out = Vec::new();
    for h in hmonitors() {
        let Some((device, primary)) = monitor_device(h) else {
            continue;
        };
        let Some((mode, rotation)) = current(&device) else {
            continue;
        };
        let path = paths
            .iter()
            .find(|p| source_name(p).as_deref() == Some(device.as_str()));
        let name = path.and_then(target_name).unwrap_or_else(|| device.clone());
        let (hdr_supported, hdr_on) = path.map_or((false, false), hdr);
        let (scale, scales) = path
            .and_then(scaling)
            .map_or((None, vec![]), |(c, all, _)| (Some(c), all));
        let ddc = if with_ddc {
            ddc_for(h, &format!("{device}|{name}"))
        } else {
            None
        };
        out.push(Display {
            vendor: path.and_then(target_vendor).unwrap_or_default(),
            monitor_key: monitor_key(&device),
            modes: modes(&device),
            device,
            name,
            primary,
            mode,
            rotation,
            hdr_supported,
            hdr_on,
            hdr_info: path.and_then(|p| crate::graphics::hdr_info(p.target.adapterId, p.target.id)),
            scale,
            scales,
            ddc,
        });
    }
    out.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.device.cmp(&b.device)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capability string of an iiyama ProLite X4373UHSU.
    const IIYAMA: &str = "(prot(monitor)type(LCD)model(X4373UHSU)cmds(01 02 03 07 0C E3 F3)vcp(02 04 05 06 08 0B 0C 10 12 14(01 02 04 05 06 08 0B) 16 18 1A 52 60(0F 10 11 12) 62 87 8D AC AE B2 B6 C6 C8 CA CC(02 03 04 05 06 09 14 1E) D6(01 04 05) DF FD FF)mswhql(1)asset_eep(40)mccs_ver(2.2))";

    #[test]
    fn caps_parse() {
        let c = parse_caps(IIYAMA);
        assert_eq!(c[&0x60], vec![0x0f, 0x10, 0x11, 0x12]);
        assert_eq!(c[&0x14], vec![0x01, 0x02, 0x04, 0x05, 0x06, 0x08, 0x0b]);
        assert!(c[&0x10].is_empty());
        assert!(c.contains_key(&0xff));
        assert!(
            !c.contains_key(&0xe3),
            "cmds() must not leak into vcp codes"
        );
        assert_eq!(input_name(0x11), "HDMI 1");
    }
}
