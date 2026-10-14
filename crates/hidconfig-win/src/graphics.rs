//! More display/graphics settings: HDR details and SDR brightness, Windows
//! graphics toggles (Auto HDR, VRR, windowed-game optimisations, GPU scheduling),
//! display sleep timeout, colour profiles and a gamma-based blue-light filter.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use windows::Win32::{
    Devices::Display::*,
    Foundation::LUID,
    Graphics::Gdi::*,
    UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp},
};

use crate::reg;

// ---------------------------------------------------------------------------
// HDR details and SDR content brightness

#[derive(Debug, Clone, PartialEq)]
pub struct HdrInfo {
    pub bits_per_channel: u32,
    /// "RGB", "YCbCr 4:4:4", "YCbCr 4:2:2", "YCbCr 4:2:0", "Intensity".
    pub encoding: String,
    /// SDR content brightness in nits (80-480), when HDR is on.
    pub sdr_nits: Option<u32>,
}

pub(crate) fn header<T>(kind: i32, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: DISPLAYCONFIG_DEVICE_INFO_TYPE(kind),
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    }
}

/// Undocumented counterpart of DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL; it's
/// what the Settings slider uses (layout from public reverse-engineering).
const SET_SDR_WHITE_LEVEL: i32 = 0xFFFF_FFEEu32 as i32;

#[repr(C)]
struct SetSdrWhiteLevel {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    white_level: u32,
    final_value: u8,
}

/// White level is in thousandths of 80 nits.
fn nits_from_level(level: u32) -> u32 {
    ((level as u64 * 80 + 500) / 1000) as u32
}

fn level_from_nits(nits: u32) -> u32 {
    nits * 1000 / 80
}

pub(crate) fn hdr_info(adapter: LUID, target: u32) -> Option<HdrInfo> {
    let mut c = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO.0,
            adapter,
            target,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches.
    if unsafe { DisplayConfigGetDeviceInfo(&mut c.header) } != 0 {
        return None;
    }
    let encoding = match c.colorEncoding.0 {
        0 => "RGB",
        1 => "YCbCr 4:4:4",
        2 => "YCbCr 4:2:2",
        3 => "YCbCr 4:2:0",
        4 => "Intensity",
        _ => "unknown",
    }
    .to_string();
    let mut w = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: header::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL.0,
            adapter,
            target,
        ),
        ..Default::default()
    };
    // SAFETY: header size matches.
    let sdr = (unsafe { DisplayConfigGetDeviceInfo(&mut w.header) } == 0)
        .then(|| nits_from_level(w.SDRWhiteLevel));
    Some(HdrInfo {
        bits_per_channel: c.bitsPerColorChannel,
        encoding,
        sdr_nits: sdr,
    })
}

pub(crate) fn set_sdr_nits(adapter: LUID, target: u32, nits: u32) -> Result<()> {
    if !(80..=480).contains(&nits) {
        bail!("SDR content brightness must be 80-480 nits");
    }
    let v = SetSdrWhiteLevel {
        header: header::<SetSdrWhiteLevel>(SET_SDR_WHITE_LEVEL, adapter, target),
        white_level: level_from_nits(nits),
        final_value: 1,
    };
    // SAFETY: header size describes SetSdrWhiteLevel.
    let r = unsafe { DisplayConfigSetDeviceInfo(&v.header) };
    if r != 0 {
        bail!("setting SDR content brightness failed ({r}); is HDR on?");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows graphics toggles (Settings > System > Display > Graphics)

const DX_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const DX_VALUE: &str = "DirectXUserGlobalSettings";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphicsSettings {
    /// Auto HDR for DirectX 11/12 games.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_hdr: Option<bool>,
    /// Variable refresh rate for games without native VRR support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable_refresh_rate: Option<bool>,
    /// Optimizations for windowed games (flip-model upgrade).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windowed_game_optimizations: Option<bool>,
    /// Blue-light filter colour temperature (Kelvin); 6500 or unset = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_filter_kelvin: Option<u32>,
    /// Software gamma for all displays (1.0 = unchanged; >1 brighter mid-tones).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamma: Option<f64>,
    /// Turn displays off after this many minutes on AC power (0 = never).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_off_minutes: Option<u32>,
}

/// `key=value;` pairs of DirectXUserGlobalSettings.
fn dx_pairs() -> Vec<(String, String)> {
    reg::get_string(DX_KEY, DX_VALUE)
        .unwrap_or_default()
        .split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn dx_flag(pairs: &[(String, String)], key: &str, default: bool) -> bool {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map_or(default, |(_, v)| v == "1")
}

fn set_dx_flags(changes: &[(&str, bool)]) -> Result<()> {
    let mut pairs = dx_pairs();
    for &(key, on) in changes {
        let v = if on { "1" } else { "0" }.to_string();
        match pairs.iter_mut().find(|(k, _)| k == key) {
            Some(p) => p.1 = v,
            None => pairs.push((key.to_string(), v)),
        }
    }
    let text: String = pairs.iter().map(|(k, v)| format!("{k}={v};")).collect();
    reg::set_string(DX_KEY, DX_VALUE, &text)
}

pub fn read() -> Result<GraphicsSettings> {
    let p = dx_pairs();
    Ok(GraphicsSettings {
        // Windows defaults when the value is absent.
        auto_hdr: Some(dx_flag(&p, "AutoHDREnable", false)),
        variable_refresh_rate: Some(dx_flag(&p, "VRROptimizeEnable", false)),
        windowed_game_optimizations: Some(dx_flag(&p, "SwapEffectUpgradeEnable", true)),
        warm_filter_kelvin: Some(warm_filter()),
        gamma: Some(gamma()),
        display_off_minutes: display_timeout().ok(),
    })
}

pub fn apply(s: &GraphicsSettings) -> Result<()> {
    let mut errors = Vec::new();
    let mut changes = Vec::new();
    if let Some(v) = s.auto_hdr {
        changes.push(("AutoHDREnable", v));
    }
    if let Some(v) = s.variable_refresh_rate {
        changes.push(("VRROptimizeEnable", v));
    }
    if let Some(v) = s.windowed_game_optimizations {
        changes.push(("SwapEffectUpgradeEnable", v));
    }
    if !changes.is_empty()
        && let Err(e) = set_dx_flags(&changes)
    {
        errors.push(format!("graphics toggles: {e:#}"));
    }
    if (s.warm_filter_kelvin.is_some() || s.gamma.is_some())
        && let Err(e) = set_color_ramp(
            s.warm_filter_kelvin.unwrap_or_else(warm_filter),
            s.gamma.unwrap_or_else(gamma),
        )
    {
        errors.push(format!("colour ramp: {e:#}"));
    }
    if let Some(m) = s.display_off_minutes
        && let Err(e) = set_display_timeout(m)
    {
        errors.push(format!("display timeout: {e:#}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

// ---------------------------------------------------------------------------
// Hardware-accelerated GPU scheduling (machine-wide, admin + restart)

const GFX_KEY: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";

/// `None` = not supported / not set by the driver.
pub fn gpu_scheduling() -> Option<bool> {
    windows_registry::LOCAL_MACHINE
        .open(GFX_KEY)
        .ok()?
        .get_u32("HwSchMode")
        .ok()
        .map(|v| v == 2)
}

/// Needs administrator rights; takes effect after a restart.
pub fn set_gpu_scheduling(on: bool) -> Result<()> {
    windows_registry::LOCAL_MACHINE
        .create(GFX_KEY)
        .context("opening GraphicsDrivers key (run as administrator)")?
        .set_u32("HwSchMode", if on { 2 } else { 1 })
        .context("writing HwSchMode (run as administrator)")
}

// ---------------------------------------------------------------------------
// Display sleep (power plan)

mod power {
    use windows::{
        Win32::System::Power::{
            PowerGetActiveScheme, PowerReadACValueIndex, PowerSetActiveScheme,
            PowerWriteACValueIndex, PowerWriteDCValueIndex,
        },
        core::GUID,
    };

    const VIDEO_SUBGROUP: GUID = GUID::from_u128(0x7516b95f_f776_4464_8c53_06167f40cc99);
    const VIDEO_IDLE: GUID = GUID::from_u128(0x3c0bc021_c8a8_4e07_a973_6b14cbcb2b7e);

    fn scheme() -> anyhow::Result<GUID> {
        let mut p: *mut GUID = std::ptr::null_mut();
        // SAFETY: the API allocates the GUID with LocalAlloc; we copy and free it.
        unsafe {
            PowerGetActiveScheme(None, &mut p).ok()?;
            let g = *p;
            let _ = windows::Win32::Foundation::LocalFree(Some(
                windows::Win32::Foundation::HLOCAL(p as _),
            ));
            Ok(g)
        }
    }

    pub fn read() -> anyhow::Result<u32> {
        let s = scheme()?;
        let mut secs = 0u32;
        // SAFETY: plain out value.
        unsafe {
            PowerReadACValueIndex(
                None,
                Some(&s),
                Some(&VIDEO_SUBGROUP),
                Some(&VIDEO_IDLE),
                &mut secs,
            )
            .ok()?
        };
        Ok(secs)
    }

    pub fn write(secs: u32) -> anyhow::Result<()> {
        let s = scheme()?;
        // SAFETY: plain calls on the user's active scheme (no admin needed).
        unsafe {
            PowerWriteACValueIndex(None, &s, Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), secs)
                .ok()?;
            let r =
                PowerWriteDCValueIndex(None, &s, Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), secs);
            anyhow::ensure!(r == 0, "PowerWriteDCValueIndex failed ({r})");
            PowerSetActiveScheme(None, Some(&s)).ok()?;
        }
        Ok(())
    }
}

/// Minutes until displays turn off (0 = never).
pub fn display_timeout() -> Result<u32> {
    Ok(power::read()?.div_ceil(60))
}

pub fn set_display_timeout(minutes: u32) -> Result<()> {
    power::write(minutes * 60)
}

/// Turns all displays off now (they wake on input).
pub fn displays_off() {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{HWND_BROADCAST, PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND},
    };
    // SAFETY: posted broadcast; 2 = power off.
    unsafe {
        let _ = PostMessageW(
            Some(HWND_BROADCAST),
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER as usize),
            LPARAM(2),
        );
    }
}

// ---------------------------------------------------------------------------
// Blue-light filter (gamma ramp on every display)

/// RGB multipliers for a colour temperature (Tanner Helland's approximation).
pub fn kelvin_to_rgb(kelvin: u32) -> (f64, f64, f64) {
    let t = kelvin.clamp(1000, 40000) as f64 / 100.0;
    let r = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let g = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let b = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    let c = |v: f64| (v / 255.0).clamp(0.0, 1.0);
    // Normalise so 6500 K is neutral (the approximation is slightly off-white there).
    let (r65, g65, b65) = (1.0, 0.996_9, 0.981_0);
    (
        (c(r) / r65).min(1.0),
        (c(g) / g65).min(1.0),
        (c(b) / b65).min(1.0),
    )
}

fn display_dcs() -> Vec<(String, HDC)> {
    let mut out = Vec::new();
    for i in 0.. {
        let mut dev = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: cb set; index enumeration.
        if !unsafe { EnumDisplayDevicesW(None, i, &mut dev, 0) }.as_bool() {
            break;
        }
        if !dev.StateFlags.contains(DISPLAY_DEVICE_ACTIVE) {
            continue;
        }
        // SAFETY: DeviceName is a NUL-terminated buffer.
        let hdc = unsafe {
            CreateDCW(
                windows::core::w!("DISPLAY"),
                windows::core::PCWSTR(dev.DeviceName.as_ptr()),
                None,
                None,
            )
        };
        if !hdc.is_invalid() {
            let name = String::from_utf16_lossy(
                &dev.DeviceName[..dev.DeviceName.iter().position(|&c| c == 0).unwrap_or(32)],
            );
            out.push((name, hdc));
        }
    }
    out
}

fn primary_ramp() -> Option<[[u16; 256]; 3]> {
    let mut ramp = [[0u16; 256]; 3];
    let (_, hdc) = display_dcs().into_iter().next()?;
    // SAFETY: 3x256 u16 buffer as the API expects; DC deleted after.
    let ok = unsafe {
        let ok = GetDeviceGammaRamp(hdc, ramp.as_mut_ptr() as *mut _).as_bool();
        let _ = DeleteDC(hdc);
        ok
    };
    ok.then_some(ramp)
}

/// Current software gamma, estimated from the primary display's ramp.
pub fn gamma() -> f64 {
    let Some(ramp) = primary_ramp() else {
        return 1.0;
    };
    let (mid, top) = (ramp[1][128] as f64, ramp[1][255].max(1) as f64);
    let v = (mid / top).clamp(1e-6, 0.999_999);
    let g = (128.0f64 / 255.0).ln() / v.ln();
    if (g - 1.0).abs() < 0.02 {
        1.0
    } else {
        (g * 100.0).round() / 100.0
    }
}

/// Current filter temperature, estimated from the primary display's ramp (6500 = off).
pub fn warm_filter() -> u32 {
    let Some(ramp) = primary_ramp() else {
        return 6500;
    };
    let blue = ramp[2][255] as f64 / ramp[0][255].max(1) as f64;
    if blue > 0.995 {
        return 6500;
    }
    // Find the temperature whose blue multiplier matches.
    (1900..=6500)
        .step_by(100)
        .min_by_key(|&k| ((kelvin_to_rgb(k).2 - blue).abs() * 10000.0) as u32)
        .unwrap_or(6500)
}

/// Tints every display; 6500 K = neutral. Keeps the current gamma.
pub fn set_warm_filter(kelvin: u32) -> Result<()> {
    set_color_ramp(kelvin, gamma())
}

/// Sets every display's ramp from a colour temperature and a gamma (1.0 =
/// unchanged). Windows refuses ramps far from linear unless "GdiIcmGammaRange"
/// is raised, which limits how far this can go.
pub fn set_color_ramp(kelvin: u32, gamma: f64) -> Result<()> {
    let gamma = gamma.clamp(0.3, 3.0);
    let (r, g, b) = kelvin_to_rgb(kelvin.clamp(1900, 6500));
    let mut ramp = [[0u16; 256]; 3];
    for i in 0..256 {
        let base = (i as f64 / 255.0).powf(1.0 / gamma) * 65535.0;
        ramp[0][i] = (base * r) as u16;
        ramp[1][i] = (base * g) as u16;
        ramp[2][i] = (base * b) as u16;
    }
    let mut failed = Vec::new();
    for (name, hdc) in display_dcs() {
        // SAFETY: 3x256 u16 buffer; DC deleted after.
        let ok = unsafe {
            let ok = SetDeviceGammaRamp(hdc, ramp.as_ptr() as *const _).as_bool();
            let _ = DeleteDC(hdc);
            ok
        };
        if !ok {
            failed.push(name);
        }
    }
    if !failed.is_empty() {
        bail!(
            "Windows refused the colour ramp on {} (too warm for its gamma limits?)",
            failed.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Colour profiles (ICC) per monitor

#[derive(Debug, Clone, PartialEq)]
pub struct ColorProfiles {
    /// Installed display (`mntr` class) profiles, file names.
    pub installed: Vec<String>,
    /// The profile Windows uses for this monitor.
    pub default: Option<String>,
    /// Using a per-user choice instead of the system-wide one.
    pub per_user: bool,
}

mod icm {
    use windows::{
        Win32::UI::ColorSystem::*,
        core::{BOOL, PCWSTR},
    };

    /// 'mntr'
    pub const CLASS_MONITOR: u32 = 0x6d6e_7472;

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    pub fn default(key: &str, scope: WCS_PROFILE_MANAGEMENT_SCOPE) -> Option<String> {
        let k = wide(key);
        let mut size = 0u32;
        // SAFETY: two-call pattern; buffer sized in bytes as reported.
        unsafe {
            WcsGetDefaultColorProfileSize(
                scope,
                PCWSTR(k.as_ptr()),
                CPT_ICC,
                CPST_NONE,
                0,
                &mut size,
            )
            .ok()
            .ok()?;
            let mut buf = vec![0u16; (size as usize / 2).max(1)];
            WcsGetDefaultColorProfile(
                scope,
                PCWSTR(k.as_ptr()),
                CPT_ICC,
                CPST_NONE,
                0,
                size,
                windows::core::PWSTR(buf.as_mut_ptr()),
            )
            .ok()
            .ok()?;
            let s = String::from_utf16_lossy(
                &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())],
            );
            (!s.is_empty()).then_some(s)
        }
    }

    pub fn per_user(key: &str) -> bool {
        let k = wide(key);
        let mut on = BOOL(0);
        // SAFETY: plain out value.
        unsafe { WcsGetUsePerUserProfiles(PCWSTR(k.as_ptr()), CLASS_MONITOR, &mut on) }.as_bool()
            && on.as_bool()
    }

    pub fn set_per_user(key: &str, on: bool) -> windows::core::Result<()> {
        let k = wide(key);
        // SAFETY: NUL-terminated key.
        unsafe { WcsSetUsePerUserProfiles(PCWSTR(k.as_ptr()), CLASS_MONITOR, on) }.ok()
    }

    pub fn set_user_default(key: &str, profile: &str) -> windows::core::Result<()> {
        let (k, p) = (wide(key), wide(profile));
        // SAFETY: NUL-terminated strings alive for the calls.
        unsafe {
            WcsAssociateColorProfileWithDevice(
                WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
                PCWSTR(p.as_ptr()),
                PCWSTR(k.as_ptr()),
            )
            .ok()?;
            WcsSetDefaultColorProfile(
                WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
                PCWSTR(k.as_ptr()),
                CPT_ICC,
                CPST_NONE,
                0,
                PCWSTR(p.as_ptr()),
            )
            .ok()
        }
    }
}

fn color_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join(r"System32\spool\drivers\color")
}

/// Installed profiles whose ICC header says "display device".
fn installed_display_profiles() -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(color_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.ends_with(".icm") || n.ends_with(".icc")
        })
        .filter(|e| {
            use std::io::Read;
            let mut head = [0u8; 16];
            std::fs::File::open(e.path())
                .and_then(|mut f| f.read_exact(&mut head))
                .is_ok()
                && &head[12..16] == b"mntr"
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// `key`: the monitor's device key (`Display::monitor_key`).
pub fn color_profiles(key: &str) -> ColorProfiles {
    use windows::Win32::UI::ColorSystem::{
        WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
    };
    let per_user = icm::per_user(key);
    let default = if per_user {
        icm::default(key, WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER)
    } else {
        icm::default(key, WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE)
    };
    ColorProfiles {
        installed: installed_display_profiles(),
        default,
        per_user,
    }
}

/// Makes `profile` this user's profile for the monitor (`None`: back to the
/// system-wide choice). No admin needed.
pub fn set_color_profile(key: &str, profile: Option<&str>) -> Result<()> {
    match profile {
        None => icm::set_per_user(key, false).context("WcsSetUsePerUserProfiles")?,
        Some(p) => {
            icm::set_per_user(key, true).context("WcsSetUsePerUserProfiles")?;
            icm::set_user_default(key, p).with_context(|| format!("setting {p} as default"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_level_roundtrip() {
        assert_eq!(nits_from_level(1000), 80);
        assert_eq!(nits_from_level(level_from_nits(240)), 240);
    }

    #[test]
    fn kelvin_neutral_and_warm() {
        let (r, g, b) = kelvin_to_rgb(6500);
        assert!(r > 0.99 && g > 0.99 && b > 0.99);
        let (r, _, b) = kelvin_to_rgb(3400);
        assert!(r > 0.99 && b < 0.7);
    }
}
