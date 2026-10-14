//! Windows audio: endpoints, volume/mute, default devices, per-app sessions,
//! and the per-user "hidden" toggles (communications ducking, mono audio).
//!
//! Volumes and mutes are live controls (not saved): enforcing them from a profile
//! would fight the volume keys. What the profile does own: the preferred default
//! devices, ducking and mono.

use std::{collections::BTreeMap, ffi::c_void};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, PROPERTYKEY},
        Media::Audio::{Endpoints::IAudioEndpointVolume, *},
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                STGM_READ, StructuredStorage::PropVariantToStringAlloc,
            },
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
    },
    core::{GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR},
};

use crate::reg;

// ---------------------------------------------------------------------------
// IPolicyConfig: undocumented, but stable since Windows 7 and used by every
// "switch audio device" tool (EarTrumpet, SoundSwitch, NirSoft). Windows 10+ IID.

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct DeviceShareMode(pub i32);

pub(crate) use policy::IPolicyConfig;

#[allow(non_snake_case)]
pub(crate) mod policy {
    use windows::{
        Win32::{
            Foundation::PROPERTYKEY, Media::Audio::*, System::Com::StructuredStorage::PROPVARIANT,
        },
        core::{BOOL, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface},
    };

    use super::DeviceShareMode;

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
        fn GetDeviceFormat(
            &self,
            device: PCWSTR,
            default: BOOL,
            format: *mut *mut WAVEFORMATEX,
        ) -> HRESULT;
        fn ResetDeviceFormat(&self, device: PCWSTR) -> HRESULT;
        fn SetDeviceFormat(
            &self,
            device: PCWSTR,
            endpoint: *mut WAVEFORMATEX,
            mix: *mut WAVEFORMATEX,
        ) -> HRESULT;
        fn GetProcessingPeriod(
            &self,
            device: PCWSTR,
            default: BOOL,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> HRESULT;
        fn SetProcessingPeriod(&self, device: PCWSTR, period: *mut i64) -> HRESULT;
        fn GetShareMode(&self, device: PCWSTR, mode: *mut DeviceShareMode) -> HRESULT;
        fn SetShareMode(&self, device: PCWSTR, mode: *mut DeviceShareMode) -> HRESULT;
        fn GetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: BOOL,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
        fn SetPropertyValue(
            &self,
            device: PCWSTR,
            fx_store: BOOL,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
        fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> HRESULT;
        fn SetEndpointVisibility(&self, device: PCWSTR, visible: BOOL) -> HRESULT;
    }

    // The macro's methods are module-private; these are the crate-visible calls.
    // SAFETY (all): `device` must be a NUL-terminated endpoint id alive for the call.
    impl IPolicyConfig {
        pub(crate) unsafe fn set_default_endpoint(&self, device: PCWSTR, role: ERole) -> HRESULT {
            unsafe { self.SetDefaultEndpoint(device, role) }
        }
        pub(crate) unsafe fn get_property(
            &self,
            device: PCWSTR,
            fx_store: bool,
            key: &PROPERTYKEY,
            value: &mut PROPVARIANT,
        ) -> HRESULT {
            unsafe { self.GetPropertyValue(device, fx_store.into(), key, value) }
        }
        pub(crate) unsafe fn set_property(
            &self,
            device: PCWSTR,
            fx_store: bool,
            key: &PROPERTYKEY,
            value: &mut PROPVARIANT,
        ) -> HRESULT {
            unsafe { self.SetPropertyValue(device, fx_store.into(), key, value) }
        }
        pub(crate) unsafe fn device_format(
            &self,
            device: PCWSTR,
            default: bool,
            out: *mut *mut WAVEFORMATEX,
        ) -> HRESULT {
            unsafe { self.GetDeviceFormat(device, default.into(), out) }
        }
        pub(crate) unsafe fn set_device_format(
            &self,
            device: PCWSTR,
            endpoint: *mut WAVEFORMATEX,
            mix: *mut WAVEFORMATEX,
        ) -> HRESULT {
            unsafe { self.SetDeviceFormat(device, endpoint, mix) }
        }
    }
}

pub(crate) fn policy_config() -> Result<IPolicyConfig> {
    com_init();
    // SAFETY: standard COM activation of an in-proc class.
    let unk: IUnknown = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
        .context("creating PolicyConfigClient")?;
    unk.cast()
        .context("IPolicyConfig not available on this Windows build")
}

// ---------------------------------------------------------------------------

/// Joins the calling thread to the multithreaded apartment (idempotent).
pub fn com_init() {
    thread_local!(static INIT: () = {
        // SAFETY: per-thread COM init; S_FALSE/RPC_E_CHANGED_MODE are fine to ignore.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    });
    INIT.with(|_| {});
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn edataflow(self) -> EDataFlow {
        match self {
            Flow::Output => eRender,
            Flow::Input => eCapture,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub id: String,
    /// e.g. "Speakers (Razer Hammerhead V3 HyperSpeed)".
    pub name: String,
    pub flow: Flow,
    /// Default for games/media (console + multimedia roles).
    pub is_default: bool,
    /// Default for calls (communications role).
    pub is_default_comms: bool,
    /// 0.0..=1.0
    pub volume: f32,
    pub muted: bool,
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    com_init();
    // SAFETY: standard COM activation.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("creating MMDeviceEnumerator")
}

fn take_pwstr(p: PWSTR) -> String {
    // SAFETY: COM-allocated, NUL-terminated string we own and free here.
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn device_id(d: &IMMDevice) -> Result<String> {
    // SAFETY: COM call returning an owned string.
    Ok(take_pwstr(unsafe { d.GetId() }?))
}

fn friendly_name(d: &IMMDevice) -> Result<String> {
    // SAFETY: COM calls; the PROPVARIANT is dropped (cleared) by its Drop impl.
    unsafe {
        let store = d.OpenPropertyStore(STGM_READ)?;
        let v = store.GetValue(&PKEY_Device_FriendlyName)?;
        Ok(take_pwstr(PropVariantToStringAlloc(&v)?))
    }
}

fn endpoint_volume(d: &IMMDevice) -> Result<IAudioEndpointVolume> {
    // SAFETY: activating a documented interface on the endpoint.
    Ok(unsafe { d.Activate(CLSCTX_ALL, None) }?)
}

fn default_id(e: &IMMDeviceEnumerator, flow: Flow, role: ERole) -> Option<String> {
    // SAFETY: COM call; fails when no device of that kind exists.
    let d = unsafe { e.GetDefaultAudioEndpoint(flow.edataflow(), role) }.ok()?;
    device_id(&d).ok()
}

/// Active endpoints of both directions, outputs first.
pub fn endpoints() -> Result<Vec<Endpoint>> {
    let e = enumerator()?;
    let mut out = Vec::new();
    for flow in [Flow::Output, Flow::Input] {
        let default = default_id(&e, flow, eConsole);
        let comms = default_id(&e, flow, eCommunications);
        // SAFETY: COM enumeration.
        let coll = unsafe { e.EnumAudioEndpoints(flow.edataflow(), DEVICE_STATE_ACTIVE) }?;
        for i in 0..unsafe { coll.GetCount() }? {
            let d = unsafe { coll.Item(i) }?;
            let id = device_id(&d)?;
            let vol = endpoint_volume(&d)?;
            out.push(Endpoint {
                name: friendly_name(&d).unwrap_or_else(|_| id.clone()),
                flow,
                is_default: default.as_deref() == Some(id.as_str()),
                is_default_comms: comms.as_deref() == Some(id.as_str()),
                // SAFETY: COM calls on a live interface.
                volume: unsafe { vol.GetMasterVolumeLevelScalar() }?,
                muted: unsafe { vol.GetMute() }?.as_bool(),
                id,
            });
        }
    }
    out.sort_by(|a, b| (a.flow == Flow::Input, &a.name).cmp(&(b.flow == Flow::Input, &b.name)));
    Ok(out)
}

fn device(id: &str) -> Result<IMMDevice> {
    let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
    // SAFETY: NUL-terminated id buffer alive for the call.
    unsafe { enumerator()?.GetDevice(PCWSTR(wide.as_ptr())) }
        .with_context(|| format!("audio device {id}"))
}

/// Finds an active endpoint by id, exact name, or case-insensitive name substring.
pub fn find(flow: Option<Flow>, query: &str) -> Result<Endpoint> {
    let all: Vec<Endpoint> = endpoints()?
        .into_iter()
        .filter(|e| flow.is_none_or(|f| e.flow == f))
        .collect();
    if let Some(e) = all.iter().find(|e| e.id == query || e.name == query) {
        return Ok(e.clone());
    }
    let q = query.to_lowercase();
    let hits: Vec<_> = all
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&q))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no active audio device matches {query:?}"),
        many => bail!(
            "{query:?} matches several devices: {}",
            many.iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn set_volume(id: &str, level: f32) -> Result<()> {
    let v = endpoint_volume(&device(id)?)?;
    // SAFETY: COM call; no event context.
    unsafe { v.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null()) }?;
    Ok(())
}

pub fn set_mute(id: &str, muted: bool) -> Result<()> {
    let v = endpoint_volume(&device(id)?)?;
    // SAFETY: COM call; no event context.
    unsafe { v.SetMute(muted, std::ptr::null()) }?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Games, media, system sounds (console + multimedia).
    Default,
    /// Calls.
    Communications,
}

pub fn set_default(id: &str, role: Role) -> Result<()> {
    let pc = policy_config()?;
    let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
    let roles: &[ERole] = match role {
        Role::Default => &[eConsole, eMultimedia],
        Role::Communications => &[eCommunications],
    };
    for &r in roles {
        // SAFETY: NUL-terminated id alive for the call.
        unsafe { pc.set_default_endpoint(PCWSTR(wide.as_ptr()), r) }
            .ok()
            .context("SetDefaultEndpoint")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-app sessions

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub endpoint_id: String,
    pub pid: u32,
    /// Executable file name ("chrome.exe"), or "System sounds".
    pub app: String,
    pub volume: f32,
    pub muted: bool,
}

fn process_name(pid: u32) -> Option<String> {
    // SAFETY: handle opened with limited rights and closed below; buffer sized.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(path.rsplit('\\').next().unwrap_or(&path).to_string())
    }
}

fn session_controls(endpoint_id: &str) -> Result<Vec<IAudioSessionControl2>> {
    let d = device(endpoint_id)?;
    // SAFETY: COM activation and enumeration.
    unsafe {
        let mgr: IAudioSessionManager2 = d.Activate(CLSCTX_ALL, None)?;
        let en = mgr.GetSessionEnumerator()?;
        let mut out = Vec::new();
        for i in 0..en.GetCount()? {
            out.push(en.GetSession(i)?.cast::<IAudioSessionControl2>()?);
        }
        Ok(out)
    }
}

/// Live sessions (not expired) on every active output.
pub fn sessions() -> Result<Vec<Session>> {
    let mut out = Vec::new();
    for ep in endpoints()?.into_iter().filter(|e| e.flow == Flow::Output) {
        for c in session_controls(&ep.id).unwrap_or_default() {
            // SAFETY: COM calls on live session interfaces.
            unsafe {
                if c.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                let pid = c.GetProcessId().unwrap_or(0);
                let system = c.IsSystemSoundsSession() == HRESULT(0);
                let app = if system {
                    "System sounds".to_string()
                } else {
                    process_name(pid).unwrap_or_else(|| format!("pid {pid}"))
                };
                let Ok(v) = c.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                out.push(Session {
                    endpoint_id: ep.id.clone(),
                    pid,
                    app,
                    volume: v.GetMasterVolume().unwrap_or(1.0),
                    muted: v.GetMute().map(|m| m.as_bool()).unwrap_or(false),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.app
            .to_lowercase()
            .cmp(&b.app.to_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    // One row per app: set_session acts on every output anyway.
    out.dedup_by(|a, b| a.pid == b.pid && a.app == b.app);
    Ok(out)
}

/// Sets volume and/or mute of every session of `pid` (or all system-sound sessions when pid 0).
pub fn set_session(pid: u32, volume: Option<f32>, muted: Option<bool>) -> Result<usize> {
    let mut n = 0;
    for ep in endpoints()?.into_iter().filter(|e| e.flow == Flow::Output) {
        for c in session_controls(&ep.id).unwrap_or_default() {
            // SAFETY: COM calls on live session interfaces.
            unsafe {
                if c.GetProcessId().unwrap_or(0) != pid {
                    continue;
                }
                let v = c.cast::<ISimpleAudioVolume>()?;
                if let Some(level) = volume {
                    v.SetMasterVolume(level.clamp(0.0, 1.0), std::ptr::null())?;
                }
                if let Some(m) = muted {
                    v.SetMute(m, std::ptr::null())?;
                }
                n += 1;
            }
        }
    }
    if n == 0 {
        bail!("no audio session for pid {pid}");
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Spatial sound (Windows Sonic, Dolby Atmos, DTS:X). Reading uses the documented
// WinRT SpatialAudioDeviceConfiguration.

/// Known formats: config name, display name, WinRT getter.
pub const SPATIAL_FORMATS: &[(
    &str,
    &str,
    fn() -> windows::core::Result<windows::core::HSTRING>,
)] = {
    use windows::Media::Audio::SpatialAudioFormatSubtype as S;
    &[
        (
            "windows-sonic",
            "Windows Sonic for Headphones",
            S::WindowsSonic,
        ),
        (
            "dolby-atmos-headphones",
            "Dolby Atmos for Headphones",
            S::DolbyAtmosForHeadphones,
        ),
        (
            "dolby-atmos-home-theater",
            "Dolby Atmos for Home Theater",
            S::DolbyAtmosForHomeTheater,
        ),
        (
            "dolby-atmos-speakers",
            "Dolby Atmos for Speakers",
            S::DolbyAtmosForSpeakers,
        ),
        ("dts-headphone-x", "DTS Headphone:X", S::DTSHeadphoneX),
        ("dts-x-ultra", "DTS:X Ultra", S::DTSXUltra),
        (
            "dts-x-home-theater",
            "DTS:X for Home Theater",
            S::DTSXForHomeTheater,
        ),
    ]
};

#[derive(Debug, Clone, PartialEq)]
pub struct SpatialState {
    pub supported: bool,
    /// Config names of the formats this device can use.
    pub available: Vec<&'static str>,
    /// What the user selected (`None` = off).
    pub selected: Option<String>,
    /// What is actually running (can differ, e.g. an expired Dolby licence).
    pub active: Option<String>,
}

/// WinRT device id for an MMDevice render endpoint id.
fn winrt_render_id(mm_id: &str) -> String {
    format!(r"\\?\SWD#MMDEVAPI#{mm_id}#{{e6327cad-dcec-4949-ae8a-991e976a79d2}}")
}

/// Config name for a format GUID string (unknown GUIDs are returned as-is).
fn format_name(guid: &str) -> Option<String> {
    if guid.is_empty() || guid == "{00000000-0000-0000-0000-000000000000}" {
        return None;
    }
    let g = guid.to_lowercase();
    SPATIAL_FORMATS
        .iter()
        .find(|(_, _, f)| f().is_ok_and(|h| h.to_string().to_lowercase() == g))
        .map(|(n, _, _)| n.to_string())
        .or_else(|| Some(guid.to_string()))
}

pub fn spatial(endpoint_id: &str) -> Result<SpatialState> {
    use windows::{Media::Audio::SpatialAudioDeviceConfiguration, core::HSTRING};
    com_init();
    let cfg = SpatialAudioDeviceConfiguration::GetForDeviceId(&HSTRING::from(winrt_render_id(
        endpoint_id,
    )))
    .context("SpatialAudioDeviceConfiguration")?;
    let available = SPATIAL_FORMATS
        .iter()
        .filter(|(_, _, f)| {
            f().and_then(|g| cfg.IsSpatialAudioFormatSupported(&g))
                .unwrap_or(false)
        })
        .map(|(n, _, _)| *n)
        .collect();
    Ok(SpatialState {
        supported: cfg.IsSpatialAudioSupported()?,
        available,
        selected: format_name(&cfg.DefaultSpatialAudioFormat()?.to_string()),
        active: format_name(&cfg.ActiveSpatialAudioFormat()?.to_string()),
    })
}

/// Selects a spatial format (`None` or "off" = off) on an output endpoint, via the
/// documented `SetDefaultSpatialAudioFormatAsync`. Microsoft documents it as
/// "caller must own the format", but Windows 11 allows it for desktop apps
/// (verified for Windows Sonic, Dolby Atmos for Headphones, DTS Headphone:X, off).
pub fn set_spatial(endpoint_id: &str, format: Option<&str>) -> Result<()> {
    use windows::{
        Media::Audio::{SetDefaultSpatialAudioFormatStatus as St, SpatialAudioDeviceConfiguration},
        core::HSTRING,
    };
    com_init();
    let format = format.filter(|f| *f != "off");
    let guid = match format {
        None => HSTRING::from("{00000000-0000-0000-0000-000000000000}"),
        Some(name) => {
            let (_, _, f) = SPATIAL_FORMATS
                .iter()
                .find(|(n, _, _)| *n == name)
                .with_context(|| format!("unknown spatial format {name:?}"))?;
            f()?
        }
    };
    let cfg = SpatialAudioDeviceConfiguration::GetForDeviceId(&HSTRING::from(winrt_render_id(
        endpoint_id,
    )))?;
    let status = cfg
        .SetDefaultSpatialAudioFormatAsync(&guid)?
        .join()?
        .Status()?;
    let why = match status {
        St::Succeeded => return Ok(()),
        St::AccessDenied => "access denied",
        St::LicenseExpired => "the format's licence has expired (open its app, e.g. Dolby Access)",
        St::LicenseNotValidForAudioEndpoint => "the licence doesn't cover this device",
        St::NotSupportedOnAudioEndpoint => "not supported on this device",
        _ => "unknown error",
    };
    bail!("{}: {why}", format.map_or("off", spatial_display_name))
}

pub fn spatial_display_name(name: &str) -> &str {
    SPATIAL_FORMATS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map_or(name, |(_, d, _)| d)
}

// ---------------------------------------------------------------------------
// Per-device options (Sound control panel > device > Properties > Advanced /
// Enhancements). Written through IPolicyConfig, i.e. by the audio service, so no
// elevation is needed.

/// "Allow applications to take exclusive control of this device".
const PKEY_EXCLUSIVE_ALLOWED: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
    pid: 3,
};
/// "Give exclusive mode applications priority".
const PKEY_EXCLUSIVE_PRIORITY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
    pid: 4,
};
/// PKEY_AudioEndpoint_Disable_SysFx (in the FX store): 1 = enhancements off.
const PKEY_DISABLE_SYSFX: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e),
    pid: 5,
};

/// Sample rates and bit depths Windows offers in the "Default Format" list.
const RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
const DEPTHS: [u16; 3] = [16, 24, 32];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleFormat {
    pub rate: u32,
    pub bits: u16,
}

impl std::fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} bit, {} Hz", self.bits, self.rate)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceOptions {
    pub channels: u16,
    pub format: Option<SampleFormat>,
    /// Formats the device accepts (what the Default Format dropdown would list).
    pub formats: Vec<SampleFormat>,
    pub exclusive_allowed: bool,
    pub exclusive_priority: bool,
    /// `None`: the device has no enhancement (FX) store.
    pub enhancements: Option<bool>,
}

fn get_u32_prop(pc: &IPolicyConfig, id: &[u16], fx: bool, key: &PROPERTYKEY) -> Result<Option<u32>> {
    use windows::Win32::System::{
        Com::StructuredStorage::{PROPVARIANT, PropVariantClear},
        Variant::{VT_EMPTY, VT_UI4},
    };
    let mut pv = PROPVARIANT::default();
    // SAFETY: id is NUL-terminated; pv is read per its vt, then cleared.
    unsafe {
        let hr = pc.get_property(PCWSTR(id.as_ptr()), fx, key, &mut pv);
        if hr.is_err() {
            return Ok(None);
        }
        let inner = &pv.Anonymous.Anonymous;
        let v = match inner.vt {
            VT_UI4 => Some(inner.Anonymous.ulVal),
            VT_EMPTY => None,
            vt => {
                let _ = PropVariantClear(&mut pv);
                bail!("unexpected property type {}", vt.0)
            }
        };
        let _ = PropVariantClear(&mut pv);
        Ok(v)
    }
}

fn set_u32_prop(pc: &IPolicyConfig, id: &[u16], fx: bool, key: &PROPERTYKEY, v: u32) -> Result<()> {
    use windows::Win32::System::{Com::StructuredStorage::PROPVARIANT, Variant::VT_UI4};
    let mut pv = PROPVARIANT::default();
    // SAFETY: a plain VT_UI4 owns nothing.
    unsafe {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
        pc.set_property(PCWSTR(id.as_ptr()), fx, key, &mut pv).ok().context("SetPropertyValue")?;
    }
    Ok(())
}

/// Current device ("Default Format") format from IPolicyConfig.
fn device_format(pc: &IPolicyConfig, id: &[u16]) -> Result<WAVEFORMATEXTENSIBLE> {
    let mut p: *mut WAVEFORMATEX = std::ptr::null_mut();
    // SAFETY: the callee allocates with CoTaskMemAlloc; we copy and free it.
    unsafe {
        pc.device_format(PCWSTR(id.as_ptr()), false, &mut p).ok().context("GetDeviceFormat")?;
        if p.is_null() {
            bail!("no device format");
        }
        let mut out = WAVEFORMATEXTENSIBLE::default();
        let len = (size_of::<WAVEFORMATEX>() + (*p).cbSize as usize).min(size_of::<WAVEFORMATEXTENSIBLE>());
        std::ptr::copy_nonoverlapping(p as *const u8, &mut out as *mut _ as *mut u8, len);
        CoTaskMemFree(Some(p as *const c_void));
        Ok(out)
    }
}

/// A PCM WAVEFORMATEXTENSIBLE like the one Windows writes for "Default Format".
fn pcm_format(channels: u16, mask: u32, rate: u32, bits: u16) -> WAVEFORMATEXTENSIBLE {
    pcm_format_in(channels, mask, rate, bits, bits)
}

/// `container` bits per sample holding `valid` bits (24-in-32 is common).
fn pcm_format_in(channels: u16, mask: u32, rate: u32, valid: u16, container: u16) -> WAVEFORMATEXTENSIBLE {
    use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
    let block = channels * container.div_ceil(8);
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: container,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: valid },
        dwChannelMask: mask,
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
    }
}

fn current_layout(f: &WAVEFORMATEXTENSIBLE) -> (u16, u32) {
    let channels = f.Format.nChannels.max(1);
    // Fall back to stereo/mono speaker masks for plain WAVEFORMATEX formats.
    let mask = if f.Format.cbSize >= 22 { f.dwChannelMask } else if channels == 1 { 0x4 } else { 0x3 };
    (channels, mask)
}

fn audio_client(endpoint_id: &str) -> Result<IAudioClient> {
    // SAFETY: activating a documented interface on the endpoint.
    Ok(unsafe { device(endpoint_id)?.Activate(CLSCTX_ALL, None) }?)
}

/// The exclusive-mode format the device accepts for `rate`/`bits`, trying a packed
/// container first and then the next larger one (24-in-32, 16-in-32 ...).
fn accepted_format(client: &IAudioClient, channels: u16, mask: u32, rate: u32, bits: u16) -> Option<WAVEFORMATEXTENSIBLE> {
    [bits, 32].into_iter().filter(|c| *c >= bits).find_map(|container| {
        let f = pcm_format_in(channels, mask, rate, bits, container);
        // SAFETY: f is fully initialised; no closest-match out param.
        unsafe { client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &f.Format, None) }
            .is_ok()
            .then_some(f)
    })
}

pub fn device_options(endpoint_id: &str) -> Result<DeviceOptions> {
    let pc = policy_config()?;
    let id = wide(endpoint_id);
    let fmt = device_format(&pc, &id).ok();
    let (channels, mask) = fmt.as_ref().map_or((2, 0x3), current_layout);
    let format = fmt.map(|f| SampleFormat {
        rate: f.Format.nSamplesPerSec,
        // Valid bits (24-in-32 shows as 24, like the control panel).
        bits: if f.Format.cbSize >= 22 {
            // SAFETY: union field written by Windows for extensible formats.
            unsafe { f.Samples.wValidBitsPerSample }.max(1)
        } else {
            f.Format.wBitsPerSample
        },
    });
    // What the device accepts in exclusive mode is what the dropdown offers.
    let mut formats = Vec::new();
    if let Ok(client) = audio_client(endpoint_id) {
        for bits in DEPTHS {
            for rate in RATES {
                if accepted_format(&client, channels, mask, rate, bits).is_some() {
                    formats.push(SampleFormat { rate, bits });
                }
            }
        }
    }
    let exclusive_allowed = get_u32_prop(&pc, &id, false, &PKEY_EXCLUSIVE_ALLOWED)?.unwrap_or(1) != 0;
    let exclusive_priority = get_u32_prop(&pc, &id, false, &PKEY_EXCLUSIVE_PRIORITY)?.unwrap_or(1) != 0;
    let enhancements = get_u32_prop(&pc, &id, true, &PKEY_DISABLE_SYSFX)?.map(|d| d == 0);
    Ok(DeviceOptions { channels, format, formats, exclusive_allowed, exclusive_priority, enhancements })
}

pub fn set_device_format(endpoint_id: &str, f: SampleFormat) -> Result<()> {
    let pc = policy_config()?;
    let id = wide(endpoint_id);
    let opts = device_options(endpoint_id)?;
    if !opts.formats.contains(&f) {
        bail!("{f} is not supported by this device");
    }
    let current = device_format(&pc, &id)?;
    let (channels, mask) = current_layout(&current);
    // Same container the device accepts in exclusive mode (what the control panel uses).
    let mut fmt = audio_client(endpoint_id)
        .ok()
        .and_then(|c| accepted_format(&c, channels, mask, f.rate, f.bits))
        .unwrap_or_else(|| pcm_format(channels, mask, f.rate, f.bits));
    let mut mix = fmt;
    // SAFETY: both formats are fully initialised and outlive the call.
    unsafe {
        pc.set_device_format(PCWSTR(id.as_ptr()), &mut fmt.Format, &mut mix.Format)
            .ok()
            .context("SetDeviceFormat")?;
    }
    Ok(())
}

pub fn set_exclusive(endpoint_id: &str, allowed: Option<bool>, priority: Option<bool>) -> Result<()> {
    let pc = policy_config()?;
    let id = wide(endpoint_id);
    if let Some(a) = allowed {
        set_u32_prop(&pc, &id, false, &PKEY_EXCLUSIVE_ALLOWED, a as u32)?;
    }
    if let Some(p) = priority {
        set_u32_prop(&pc, &id, false, &PKEY_EXCLUSIVE_PRIORITY, p as u32)?;
    }
    Ok(())
}

pub fn set_enhancements(endpoint_id: &str, on: bool) -> Result<()> {
    let pc = policy_config()?;
    let id = wide(endpoint_id);
    if get_u32_prop(&pc, &id, true, &PKEY_DISABLE_SYSFX)?.is_none() {
        bail!("this device has no audio enhancements");
    }
    set_u32_prop(&pc, &id, true, &PKEY_DISABLE_SYSFX, (!on) as u32)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

// ---------------------------------------------------------------------------
// Profile-owned audio settings

const AUDIO_KEY: &str = r"Software\Microsoft\Multimedia\Audio";

/// What Windows does to other sounds while you're in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ducking {
    MuteOthers,
    Reduce80,
    Reduce50,
    DoNothing,
}

impl Ducking {
    pub const ALL: [Ducking; 4] = [
        Ducking::MuteOthers,
        Ducking::Reduce80,
        Ducking::Reduce50,
        Ducking::DoNothing,
    ];

    fn code(self) -> u32 {
        self as u32
    }

    fn from_code(c: u32) -> Option<Self> {
        Ducking::ALL.get(c as usize).copied()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    /// Preferred default output (device name); applied whenever it is connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_input: Option<String>,
    /// Defaults for calls; unset = same as above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comms_output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comms_input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ducking: Option<Ducking>,
    /// Mix stereo to mono (accessibility).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono: Option<bool>,
    /// Per-device options, keyed by device name. Applied whenever the device is connected.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub devices: BTreeMap<String, DeviceSettings>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceSettings {
    /// Output only: "windows-sonic", "dolby-atmos-headphones", "dts-headphone-x", ... or "off".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spatial: Option<String>,
    /// Default format, e.g. 48000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enhancements: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_priority: Option<bool>,
}

impl DeviceSettings {
    pub fn is_empty(&self) -> bool {
        *self == DeviceSettings::default()
    }
}

/// Current values of everything [`DeviceSettings`] covers, for one endpoint.
pub fn read_device(e: &Endpoint) -> DeviceSettings {
    let opts = device_options(&e.id).ok();
    let spatial = (e.flow == Flow::Output)
        .then(|| spatial(&e.id).ok())
        .flatten()
        .filter(|s| s.supported)
        .map(|s| s.selected.unwrap_or_else(|| "off".into()));
    DeviceSettings {
        spatial,
        sample_rate: opts.as_ref().and_then(|o| o.format).map(|f| f.rate),
        bit_depth: opts.as_ref().and_then(|o| o.format).map(|f| f.bits),
        enhancements: opts.as_ref().and_then(|o| o.enhancements),
        exclusive_allowed: opts.as_ref().map(|o| o.exclusive_allowed),
        exclusive_priority: opts.as_ref().map(|o| o.exclusive_priority),
    }
}

/// Applies one device's settings, skipping values that already match.
pub fn apply_device(e: &Endpoint, s: &DeviceSettings) -> Result<()> {
    let now = read_device(e);
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(err) = r {
            errors.push(format!("{what}: {err:#}"));
        }
    };
    if let Some(f) = &s.spatial
        && now.spatial.as_deref() != Some(f.as_str())
    {
        run("spatial sound", set_spatial(&e.id, Some(f)));
    }
    if s.sample_rate.is_some() || s.bit_depth.is_some() {
        let want = SampleFormat {
            rate: s.sample_rate.or(now.sample_rate).unwrap_or(48_000),
            bits: s.bit_depth.or(now.bit_depth).unwrap_or(24),
        };
        if Some(want.rate) != now.sample_rate || Some(want.bits) != now.bit_depth {
            run("format", set_device_format(&e.id, want));
        }
    }
    if let Some(on) = s.enhancements
        && now.enhancements != Some(on)
    {
        run("enhancements", set_enhancements(&e.id, on));
    }
    let ex_a = s.exclusive_allowed.filter(|v| now.exclusive_allowed != Some(*v));
    let ex_p = s.exclusive_priority.filter(|v| now.exclusive_priority != Some(*v));
    if ex_a.is_some() || ex_p.is_some() {
        run("exclusive mode", set_exclusive(&e.id, ex_a, ex_p));
    }
    if errors.is_empty() { Ok(()) } else { bail!(errors.join("; ")) }
}

pub fn read() -> Result<AudioSettings> {
    let eps = endpoints().unwrap_or_default();
    let name = |flow, comms: bool| {
        eps.iter()
            .find(|e| {
                e.flow == flow
                    && if comms {
                        e.is_default_comms
                    } else {
                        e.is_default
                    }
            })
            .map(|e| e.name.clone())
    };
    Ok(AudioSettings {
        default_output: name(Flow::Output, false),
        default_input: name(Flow::Input, false),
        comms_output: name(Flow::Output, true),
        comms_input: name(Flow::Input, true),
        ducking: Ducking::from_code(
            reg::get_dword(AUDIO_KEY, "UserDuckingPreference").unwrap_or(1),
        ),
        mono: Some(reg::get_dword(AUDIO_KEY, "AccessibilityMonoMixState").unwrap_or(0) != 0),
        devices: eps
            .iter()
            .map(|e| (e.name.clone(), read_device(e)))
            .filter(|(_, d)| !d.is_empty())
            .collect(),
    })
}

/// Applies set fields. Preferred devices that aren't connected are skipped
/// silently (they're applied when they show up).
pub fn apply(s: &AudioSettings) -> Result<()> {
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what}: {e:#}"));
        }
    };
    let targets = [
        (
            &s.default_output,
            Flow::Output,
            Role::Default,
            "default output",
        ),
        (
            &s.default_input,
            Flow::Input,
            Role::Default,
            "default input",
        ),
        (
            &s.comms_output,
            Flow::Output,
            Role::Communications,
            "communications output",
        ),
        (
            &s.comms_input,
            Flow::Input,
            Role::Communications,
            "communications input",
        ),
    ];
    for (name, flow, role, what) in targets {
        let Some(name) = name else { continue };
        if let Ok(ep) = find(Some(flow), name) {
            let current = if role == Role::Default {
                ep.is_default
            } else {
                ep.is_default_comms
            };
            if !current {
                run(what, set_default(&ep.id, role));
            }
        }
    }
    if let Some(d) = s.ducking {
        run(
            "ducking",
            reg::set_dword(AUDIO_KEY, "UserDuckingPreference", d.code()),
        );
    }
    if let Some(m) = s.mono {
        run(
            "mono audio",
            reg::set_dword(AUDIO_KEY, "AccessibilityMonoMixState", m as u32),
        );
    }
    if !s.devices.is_empty() {
        let eps = endpoints().unwrap_or_default();
        for (name, d) in &s.devices {
            // Not connected right now: applied when it shows up.
            if let Some(e) = eps.iter().find(|e| &e.name == name) {
                run(name, apply_device(e, d));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ducking_codes() {
        for d in Ducking::ALL {
            assert_eq!(Ducking::from_code(d.code()), Some(d));
        }
        assert_eq!(
            Ducking::from_code(1),
            Some(Ducking::Reduce80),
            "Windows default"
        );
    }
}
