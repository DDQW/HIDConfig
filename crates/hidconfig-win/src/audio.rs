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
        Foundation::CloseHandle,
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
        pub(crate) unsafe fn mix_format(
            &self,
            device: PCWSTR,
            out: *mut *mut WAVEFORMATEX,
        ) -> HRESULT {
            unsafe { self.GetMixFormat(device, out) }
        }
        pub(crate) unsafe fn set_device_format(
            &self,
            device: PCWSTR,
            endpoint: *mut WAVEFORMATEX,
            mix: *mut WAVEFORMATEX,
        ) -> HRESULT {
            unsafe { self.SetDeviceFormat(device, endpoint, mix) }
        }
        pub(crate) unsafe fn set_visibility(&self, device: PCWSTR, visible: bool) -> HRESULT {
            unsafe { self.SetEndpointVisibility(device, visible.into()) }
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
pub const SPATIAL_FORMATS: &[(&str, &str, fn() -> windows::core::Result<windows::core::HSTRING>)] = {
    use windows::Media::Audio::SpatialAudioFormatSubtype as S;
    &[
        ("windows-sonic", "Windows Sonic for Headphones", S::WindowsSonic),
        ("dolby-atmos-headphones", "Dolby Atmos for Headphones", S::DolbyAtmosForHeadphones),
        ("dolby-atmos-home-theater", "Dolby Atmos for Home Theater", S::DolbyAtmosForHomeTheater),
        ("dolby-atmos-speakers", "Dolby Atmos for Speakers", S::DolbyAtmosForSpeakers),
        ("dts-headphone-x", "DTS Headphone:X", S::DTSHeadphoneX),
        ("dts-x-ultra", "DTS:X Ultra", S::DTSXUltra),
        ("dts-x-home-theater", "DTS:X for Home Theater", S::DTSXForHomeTheater),
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
    let cfg = SpatialAudioDeviceConfiguration::GetForDeviceId(&HSTRING::from(winrt_render_id(endpoint_id)))
        .context("SpatialAudioDeviceConfiguration")?;
    let available = SPATIAL_FORMATS
        .iter()
        .filter(|(_, _, f)| f().and_then(|g| cfg.IsSpatialAudioFormatSupported(&g)).unwrap_or(false))
        .map(|(n, _, _)| *n)
        .collect();
    Ok(SpatialState {
        supported: cfg.IsSpatialAudioSupported()?,
        available,
        selected: format_name(&cfg.DefaultSpatialAudioFormat()?.to_string()),
        active: format_name(&cfg.ActiveSpatialAudioFormat()?.to_string()),
    })
}

pub fn spatial_display_name(name: &str) -> &str {
    SPATIAL_FORMATS.iter().find(|(n, _, _)| *n == name).map_or(name, |(_, d, _)| d)
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
    /// Spatial sound format per output device name, e.g. "windows-sonic" or "off".
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub spatial: BTreeMap<String, String>,
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
        spatial: BTreeMap::new(),
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
