//! Game controllers (Windows.Gaming.Input), the Xbox Wireless Adapter, and the
//! per-user Windows gaming toggles (Game Bar button, Game Mode, capture).

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use windows::{
    Gaming::Input::{Gamepad, GamepadButtons, GamepadVibration, RawGameController},
    System::Power::BatteryStatus,
};

use crate::reg;

#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    /// Index into `Gamepad::Gamepads()` at the time of listing.
    pub index: u32,
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub wireless: bool,
    /// 0-100, `None` when the controller doesn't report a battery (wired, AA unknown).
    pub battery: Option<u8>,
    pub charging: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Reading {
    /// -1.0..=1.0, up/right positive.
    pub left: (f64, f64),
    pub right: (f64, f64),
    /// 0.0..=1.0
    pub left_trigger: f64,
    pub right_trigger: f64,
    pub buttons: u32,
}

/// Button bits in [`Reading::buttons`] with their labels (Xbox naming).
pub const BUTTONS: &[(u32, &str)] = &[
    (GamepadButtons::A.0, "A"),
    (GamepadButtons::B.0, "B"),
    (GamepadButtons::X.0, "X"),
    (GamepadButtons::Y.0, "Y"),
    (GamepadButtons::LeftShoulder.0, "LB"),
    (GamepadButtons::RightShoulder.0, "RB"),
    (GamepadButtons::View.0, "View"),
    (GamepadButtons::Menu.0, "Menu"),
    (GamepadButtons::LeftThumbstick.0, "LS"),
    (GamepadButtons::RightThumbstick.0, "RS"),
    (GamepadButtons::DPadUp.0, "Up"),
    (GamepadButtons::DPadDown.0, "Down"),
    (GamepadButtons::DPadLeft.0, "Left"),
    (GamepadButtons::DPadRight.0, "Right"),
    (GamepadButtons::Paddle1.0, "P1"),
    (GamepadButtons::Paddle2.0, "P2"),
    (GamepadButtons::Paddle3.0, "P3"),
    (GamepadButtons::Paddle4.0, "P4"),
];

fn gamepads() -> Result<Vec<Gamepad>> {
    crate::audio::com_init();
    // The first call starts enumeration; controllers show up shortly after.
    let list = Gamepad::Gamepads()?;
    Ok((0..list.Size()?)
        .filter_map(|i| list.GetAt(i).ok())
        .collect())
}

fn battery(g: &Gamepad) -> (Option<u8>, bool) {
    let Ok(report) = g.TryGetBatteryReport() else {
        return (None, false);
    };
    let charging = report.Status().is_ok_and(|s| s == BatteryStatus::Charging);
    let full = report
        .FullChargeCapacityInMilliwattHours()
        .and_then(|v| v.Value());
    let left = report
        .RemainingCapacityInMilliwattHours()
        .and_then(|v| v.Value());
    let pct = match (left, full) {
        (Ok(l), Ok(f)) if f > 0 => {
            Some(((l as f64 / f as f64) * 100.0).round().clamp(0.0, 100.0) as u8)
        }
        _ => None,
    };
    (pct, charging)
}

pub fn pads() -> Result<Vec<Pad>> {
    let mut out = Vec::new();
    for (i, g) in gamepads()?.into_iter().enumerate() {
        let raw = RawGameController::FromGameController(&g).ok();
        let (battery, charging) = battery(&g);
        out.push(Pad {
            index: i as u32,
            name: raw
                .as_ref()
                .and_then(|r| r.DisplayName().ok())
                .map(|n| n.to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Xbox controller".into()),
            vid: raw
                .as_ref()
                .and_then(|r| r.HardwareVendorId().ok())
                .unwrap_or(0),
            pid: raw
                .as_ref()
                .and_then(|r| r.HardwareProductId().ok())
                .unwrap_or(0),
            wireless: g.IsWireless().unwrap_or(false),
            battery,
            charging,
        });
    }
    Ok(out)
}

fn pad(index: u32) -> Result<Gamepad> {
    gamepads()?
        .into_iter()
        .nth(index as usize)
        .with_context(|| format!("no controller #{index}"))
}

// ---------------------------------------------------------------------------
// XInput: input and the two main motors work even when no HIDConfig window has
// focus (Windows.Gaming.Input only delivers to the foreground app).

mod xinput {
    use windows::Win32::UI::Input::XboxController::{
        XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState,
    };

    pub fn connected_slots() -> Vec<u32> {
        (0..4).filter(|&i| state(i).is_some()).collect()
    }

    pub fn state(slot: u32) -> Option<XINPUT_STATE> {
        let mut st = XINPUT_STATE::default();
        // SAFETY: plain out-struct; 0 = ERROR_SUCCESS.
        (unsafe { XInputGetState(slot, &mut st) } == 0).then_some(st)
    }

    pub fn vibrate(slot: u32, left: f64, right: f64) -> bool {
        let v = XINPUT_VIBRATION {
            wLeftMotorSpeed: (left.clamp(0.0, 1.0) * 65535.0) as u16,
            wRightMotorSpeed: (right.clamp(0.0, 1.0) * 65535.0) as u16,
        };
        // SAFETY: plain in-struct.
        unsafe { XInputSetState(slot, &v) == 0 }
    }
}

/// XInput button bit -> Windows.Gaming.Input button bit.
const XINPUT_TO_WGI: &[(u16, u32)] = &[
    (0x0001, GamepadButtons::DPadUp.0),
    (0x0002, GamepadButtons::DPadDown.0),
    (0x0004, GamepadButtons::DPadLeft.0),
    (0x0008, GamepadButtons::DPadRight.0),
    (0x0010, GamepadButtons::Menu.0),
    (0x0020, GamepadButtons::View.0),
    (0x0040, GamepadButtons::LeftThumbstick.0),
    (0x0080, GamepadButtons::RightThumbstick.0),
    (0x0100, GamepadButtons::LeftShoulder.0),
    (0x0200, GamepadButtons::RightShoulder.0),
    (0x1000, GamepadButtons::A.0),
    (0x2000, GamepadButtons::B.0),
    (0x4000, GamepadButtons::X.0),
    (0x8000, GamepadButtons::Y.0),
];

fn axis(v: i16) -> f64 {
    (v as f64 / 32767.0).clamp(-1.0, 1.0)
}

/// XInput slot for the `index`-th controller (same order as Windows.Gaming.Input
/// when only Xbox controllers are connected).
fn slot_for(index: u32) -> Option<u32> {
    xinput::connected_slots().get(index as usize).copied()
}

pub fn reading(index: u32) -> Result<Reading> {
    if let Some(st) = slot_for(index).and_then(xinput::state) {
        let g = st.Gamepad;
        let buttons = XINPUT_TO_WGI
            .iter()
            .filter(|(x, _)| g.wButtons.0 & x != 0)
            .fold(0, |acc, (_, w)| acc | w);
        return Ok(Reading {
            left: (axis(g.sThumbLX), axis(g.sThumbLY)),
            right: (axis(g.sThumbRX), axis(g.sThumbRY)),
            left_trigger: g.bLeftTrigger as f64 / 255.0,
            right_trigger: g.bRightTrigger as f64 / 255.0,
            buttons,
        });
    }
    let r = pad(index)?.GetCurrentReading()?;
    Ok(Reading {
        left: (r.LeftThumbstickX, r.LeftThumbstickY),
        right: (r.RightThumbstickX, r.RightThumbstickY),
        left_trigger: r.LeftTrigger,
        right_trigger: r.RightTrigger,
        buttons: r.Buttons.0,
    })
}

/// Motor strengths 0.0..=1.0: big left motor, small right motor, and the two
/// trigger (impulse) motors. All zero stops.
pub fn rumble(
    index: u32,
    left: f64,
    right: f64,
    left_trigger: f64,
    right_trigger: f64,
) -> Result<()> {
    let c = |v: f64| v.clamp(0.0, 1.0);
    // Main motors through XInput (focus-independent); trigger motors only exist in
    // Windows.Gaming.Input, which acts while a HIDConfig window has focus.
    if left_trigger == 0.0
        && right_trigger == 0.0
        && let Some(slot) = slot_for(index)
        && xinput::vibrate(slot, left, right)
    {
        return Ok(());
    }
    pad(index)?.SetVibration(GamepadVibration {
        LeftMotor: c(left),
        RightMotor: c(right),
        LeftTrigger: c(left_trigger),
        RightTrigger: c(right_trigger),
    })?;
    Ok(())
}

/// Resting-stick analysis from samples taken while the stick isn't touched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drift {
    /// Largest distance from centre seen at rest (0..=1).
    pub max: f64,
    /// Dead zone that would hide it, with a small safety margin, as a fraction.
    pub suggested_deadzone: f64,
}

pub fn drift(samples: &[(f64, f64)]) -> Option<Drift> {
    let max = samples
        .iter()
        .map(|(x, y)| (x * x + y * y).sqrt())
        .fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.max(d))))?;
    Some(Drift {
        max,
        suggested_deadzone: (((max + 0.02) * 100.0) - 1e-9).ceil() / 100.0,
    })
}

// ---------------------------------------------------------------------------
// Xbox Wireless Adapter

/// USB product ids of Microsoft's wireless adapters.
const ADAPTERS: &[(u16, &str)] = &[
    (0x02e6, "Xbox Wireless Adapter for Windows"),
    (0x02fe, "Xbox Wireless Adapter for Windows (v2)"),
    (0x091e, "Xbox Wireless Adapter (built-in)"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Adapter {
    pub name: String,
    pub instance_id: String,
    pub driver_version: String,
    /// "Allow the computer to turn off this device to save power".
    pub power_saving_allowed: Option<bool>,
}

/// Finds a connected adapter via the device registry (no admin needed to read).
pub fn adapter() -> Option<Adapter> {
    use windows_registry::LOCAL_MACHINE;
    for &(pid, name) in ADAPTERS {
        let base = format!(r"SYSTEM\CurrentControlSet\Enum\USB\VID_045E&PID_{pid:04X}");
        let Ok(key) = LOCAL_MACHINE.open(&base) else {
            continue;
        };
        let Ok(mut instances) = key.keys() else {
            continue;
        };
        let Some(inst) = instances.next() else {
            continue;
        };
        let dev = key.open(&inst).ok()?;
        // Enum keys stay after unplugging; only report adapters that are present.
        if !crate::devices::is_present(&format!(r"USB\VID_045E&PID_{pid:04X}\{inst}")) {
            continue;
        }
        let driver = dev.get_string("Driver").ok();
        let class = driver.as_ref().and_then(|d| {
            LOCAL_MACHINE
                .open(format!(r"SYSTEM\CurrentControlSet\Control\Class\{d}"))
                .ok()
        });
        let caps = class
            .as_ref()
            .and_then(|c| c.get_u32("PnPCapabilities").ok());
        return Some(Adapter {
            name: name.to_string(),
            instance_id: format!(r"USB\VID_045E&PID_{pid:04X}\{inst}"),
            driver_version: class
                .as_ref()
                .and_then(|c| c.get_string("DriverVersion").ok())
                .unwrap_or_default(),
            // 24 (0x18) is the value Device Manager writes when the "allow the computer
            // to turn off this device" box is cleared.
            power_saving_allowed: Some(caps.is_none_or(|c| c & 0x18 != 0x18)),
        });
    }
    None
}

/// Sets "Allow the computer to turn off this device to save power" (needs admin),
/// then restarts the adapter so the driver picks it up.
pub fn set_adapter_power_saving(a: &Adapter, allowed: bool) -> Result<()> {
    use windows_registry::LOCAL_MACHINE;
    let dev = LOCAL_MACHINE
        .open(format!(r"SYSTEM\CurrentControlSet\Enum\{}", a.instance_id))
        .context("opening adapter device key")?;
    let driver = dev.get_string("Driver").context("adapter driver key")?;
    let class = LOCAL_MACHINE
        .create(format!(r"SYSTEM\CurrentControlSet\Control\Class\{driver}"))
        .context("opening driver key (run as administrator)")?;
    class
        .set_u32("PnPCapabilities", if allowed { 0 } else { 24 })
        .context("writing PnPCapabilities (run as administrator)")?;
    let status = std::process::Command::new("pnputil.exe")
        .args(["/restart-device", &a.instance_id])
        .status()
        .context("running pnputil")?;
    if !status.success() {
        bail!("pnputil /restart-device failed ({status}); unplug and replug the adapter");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-user gaming toggles

const GAMEBAR: &str = r"Software\Microsoft\GameBar";
const GAMEDVR: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GamingSettings {
    /// The Xbox (guide) button opens Game Bar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xbox_button_opens_game_bar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_mode: Option<bool>,
    /// Game Bar screenshots / recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_capture: Option<bool>,
    /// "Record what happened" (always-on background recording).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_recording: Option<bool>,
}

pub fn read() -> Result<GamingSettings> {
    let flag = |key, name, default: u32| Some(reg::get_dword(key, name).unwrap_or(default) != 0);
    Ok(GamingSettings {
        xbox_button_opens_game_bar: flag(GAMEBAR, "UseNexusForGameBarEnabled", 1),
        game_mode: flag(GAMEBAR, "AutoGameModeEnabled", 1),
        game_capture: flag(GAMEDVR, "AppCaptureEnabled", 1),
        background_recording: flag(GAMEDVR, "HistoricalCaptureEnabled", 0),
    })
}

pub fn apply(s: &GamingSettings) -> Result<()> {
    let mut errors = Vec::new();
    for (v, key, name) in [
        (
            s.xbox_button_opens_game_bar,
            GAMEBAR,
            "UseNexusForGameBarEnabled",
        ),
        (s.game_mode, GAMEBAR, "AutoGameModeEnabled"),
        (s.game_capture, GAMEDVR, "AppCaptureEnabled"),
        (s.background_recording, GAMEDVR, "HistoricalCaptureEnabled"),
    ] {
        if let Some(on) = v
            && let Err(e) = reg::set_dword(key, name, on as u32)
        {
            errors.push(format!("{name}: {e:#}"));
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
    fn drift_math() {
        assert_eq!(drift(&[]), None);
        let d = drift(&[(0.03, 0.04), (0.0, 0.01)]).unwrap();
        assert!((d.max - 0.05).abs() < 1e-9);
        assert!((d.suggested_deadzone - 0.07).abs() < 1e-9);
    }
}
