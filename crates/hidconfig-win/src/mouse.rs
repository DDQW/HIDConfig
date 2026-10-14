//! Windows pointer settings (Settings > Mouse, Control Panel > Mouse, and the hidden ones).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use windows::{
    Win32::UI::{
        Accessibility::MOUSEKEYS, Input::KeyboardAndMouse::GetDoubleClickTime,
        WindowsAndMessaging::*,
    },
    core::BOOL,
};

use crate::{
    reg,
    spi::{self, get},
};

/// Every field is optional: `None` = leave Windows alone.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MouseSettings {
    /// 1-20; 10 = 1:1 (no scaling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer_speed: Option<u8>,
    /// "Enhance pointer precision" (acceleration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enhance_precision: Option<bool>,
    /// Lines per wheel notch; 0 = one screen at a time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_chars: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll_inactive_windows: Option<bool>,
    /// Milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_click_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_buttons: Option<bool>,

    // Hidden.
    /// Pixels the second click may be away from the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_click_area: Option<u32>,
    /// Pixels the pointer must move with a button down before a drag starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drag_threshold: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_ms: Option<u32>,
    /// Focus follows the mouse (X-Mouse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_follows_mouse: Option<bool>,
    /// With focus-follows-mouse: also bring the window to the front.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_raise: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_delay_ms: Option<u32>,

    // Pointer helpers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_while_typing: Option<bool>,
    /// Show pointer location when Ctrl is pressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locate_with_ctrl: Option<bool>,
    /// 0 = off, otherwise 2-7 trail length.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trails: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_lock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_lock_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snap_to_default_button: Option<bool>,
    /// Numpad moves the pointer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_keys: Option<bool>,
}

/// Effective multiplier for each pointer speed step (index 0 = speed 1).
pub const SPEED_MULTIPLIERS: [f32; 20] = [
    0.03125, 0.0625, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25,
    2.5, 2.75, 3.0, 3.25, 3.5,
];

pub fn speed_multiplier(speed: u8) -> Option<f32> {
    SPEED_MULTIPLIERS
        .get((speed as usize).checked_sub(1)?)
        .copied()
}

/// WHEEL_PAGESCROLL (UINT_MAX): scroll one screen per notch.
const WHEEL_PAGESCROLL: u32 = u32::MAX;
const ROUTING_KEY: &str = r"Control Panel\Desktop";
const ROUTING_VALUE: &str = "MouseWheelRouting";
const CURVE_KEY: &str = r"Control Panel\Mouse";

/// Windows' default acceleration curves (`SmoothMouseXCurve` / `YCurve`).
pub const DEFAULT_X_CURVE: [u8; 40] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x6e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x40, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x29, 0xdc, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00,
];
pub const DEFAULT_Y_CURVE: [u8; 40] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfd, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x24, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfc, 0x12, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xc0, 0xbb, 0x01, 0x00, 0x00, 0x00, 0x00,
];

fn get_bool(action: SYSTEM_PARAMETERS_INFO_ACTION) -> Result<bool> {
    Ok(get::<BOOL>(action)?.as_bool())
}

fn metric(m: SYSTEM_METRICS_INDEX) -> u32 {
    // SAFETY: plain query.
    unsafe { GetSystemMetrics(m) }.max(0) as u32
}

fn mousekeys() -> Result<MOUSEKEYS> {
    spi::get_struct(
        SPI_GETMOUSEKEYS,
        MOUSEKEYS {
            cbSize: size_of::<MOUSEKEYS>() as u32,
            ..Default::default()
        },
    )
}

/// Current values of everything in [`MouseSettings`].
pub fn read() -> Result<MouseSettings> {
    let accel: [i32; 3] = get(SPI_GETMOUSE)?;
    let lines: u32 = get(SPI_GETWHEELSCROLLLINES)?;
    Ok(MouseSettings {
        pointer_speed: Some(get::<u32>(SPI_GETMOUSESPEED)? as u8),
        enhance_precision: Some(accel[2] != 0),
        scroll_lines: Some(if lines == WHEEL_PAGESCROLL { 0 } else { lines }),
        scroll_chars: Some(get(SPI_GETWHEELSCROLLCHARS)?),
        scroll_inactive_windows: Some(reg::get_dword(ROUTING_KEY, ROUTING_VALUE).unwrap_or(2) == 2),
        // SAFETY: plain query.
        double_click_ms: Some(unsafe { GetDoubleClickTime() }),
        swap_buttons: Some(metric(SM_SWAPBUTTON) != 0),
        double_click_area: Some(metric(SM_CXDOUBLECLK)),
        drag_threshold: Some(metric(SM_CXDRAG)),
        hover_ms: Some(get(SPI_GETMOUSEHOVERTIME)?),
        focus_follows_mouse: Some(get_bool(SPI_GETACTIVEWINDOWTRACKING)?),
        focus_raise: Some(get_bool(SPI_GETACTIVEWNDTRKZORDER)?),
        focus_delay_ms: Some(get(SPI_GETACTIVEWNDTRKTIMEOUT)?),
        hide_while_typing: Some(get_bool(SPI_GETMOUSEVANISH)?),
        locate_with_ctrl: Some(get_bool(SPI_GETMOUSESONAR)?),
        trails: Some(match get::<u32>(SPI_GETMOUSETRAILS)? {
            0 | 1 => 0,
            n => n,
        }),
        click_lock: Some(get_bool(SPI_GETMOUSECLICKLOCK)?),
        click_lock_ms: Some(get(SPI_GETMOUSECLICKLOCKTIME)?),
        snap_to_default_button: Some(get_bool(SPI_GETSNAPTODEFBUTTON)?),
        mouse_keys: Some(mousekeys()?.dwFlags & MKF_MOUSEKEYSON != 0),
    })
}

/// Applies the set fields; failures are collected per field.
pub fn apply(s: &MouseSettings) -> Result<()> {
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what}: {e:#}"));
        }
    };
    if let Some(v) = s.pointer_speed {
        run(
            "pointer speed",
            if (1..=20).contains(&v) {
                spi::set_pv(SPI_SETMOUSESPEED, v as usize)
            } else {
                Err(anyhow::anyhow!("must be 1..=20"))
            },
        );
    }
    if let Some(on) = s.enhance_precision {
        // Windows' own values for on/off.
        let v: [i32; 3] = if on { [6, 10, 1] } else { [0, 0, 0] };
        run("enhance precision", spi::set_ptr(SPI_SETMOUSE, v));
    }
    if let Some(v) = s.scroll_lines {
        run(
            "scroll lines",
            spi::set_ui(
                SPI_SETWHEELSCROLLLINES,
                if v == 0 { WHEEL_PAGESCROLL } else { v },
            ),
        );
    }
    if let Some(v) = s.scroll_chars {
        run("scroll chars", spi::set_ui(SPI_SETWHEELSCROLLCHARS, v));
    }
    if let Some(on) = s.scroll_inactive_windows {
        run("scroll inactive windows", {
            let r = reg::set_dword(ROUTING_KEY, ROUTING_VALUE, if on { 2 } else { 0 });
            spi::broadcast_setting_change("WindowsThemeElement");
            r
        });
    }
    if let Some(v) = s.double_click_ms {
        run("double-click time", spi::set_ui(SPI_SETDOUBLECLICKTIME, v));
    }
    if let Some(on) = s.swap_buttons {
        run(
            "swap buttons",
            spi::set_ui(SPI_SETMOUSEBUTTONSWAP, on as u32),
        );
    }
    if let Some(v) = s.double_click_area {
        run(
            "double-click area",
            spi::set_ui(SPI_SETDOUBLECLKWIDTH, v).and(spi::set_ui(SPI_SETDOUBLECLKHEIGHT, v)),
        );
    }
    if let Some(v) = s.drag_threshold {
        run(
            "drag threshold",
            spi::set_ui(SPI_SETDRAGWIDTH, v).and(spi::set_ui(SPI_SETDRAGHEIGHT, v)),
        );
    }
    if let Some(v) = s.hover_ms {
        run("hover time", spi::set_ui(SPI_SETMOUSEHOVERTIME, v));
    }
    if let Some(on) = s.focus_follows_mouse {
        run(
            "focus follows mouse",
            spi::set_pv(SPI_SETACTIVEWINDOWTRACKING, on as usize),
        );
    }
    if let Some(on) = s.focus_raise {
        run(
            "focus raise",
            spi::set_pv(SPI_SETACTIVEWNDTRKZORDER, on as usize),
        );
    }
    if let Some(v) = s.focus_delay_ms {
        run(
            "focus delay",
            spi::set_pv(SPI_SETACTIVEWNDTRKTIMEOUT, v as usize),
        );
    }
    if let Some(on) = s.hide_while_typing {
        run(
            "hide while typing",
            spi::set_pv(SPI_SETMOUSEVANISH, on as usize),
        );
    }
    if let Some(on) = s.locate_with_ctrl {
        run(
            "locate with ctrl",
            spi::set_pv(SPI_SETMOUSESONAR, on as usize),
        );
    }
    if let Some(v) = s.trails {
        run(
            "pointer trails",
            match v {
                0 => spi::set_ui(SPI_SETMOUSETRAILS, 0),
                2..=16 => spi::set_ui(SPI_SETMOUSETRAILS, v),
                _ => Err(anyhow::anyhow!("must be 0 or 2..=16")),
            },
        );
    }
    if let Some(on) = s.click_lock {
        run(
            "click lock",
            spi::set_pv(SPI_SETMOUSECLICKLOCK, on as usize),
        );
    }
    if let Some(v) = s.click_lock_ms {
        run(
            "click lock time",
            spi::set_pv(SPI_SETMOUSECLICKLOCKTIME, v as usize),
        );
    }
    if let Some(on) = s.snap_to_default_button {
        run(
            "snap to default button",
            spi::set_ui(SPI_SETSNAPTODEFBUTTON, on as u32),
        );
    }
    if let Some(on) = s.mouse_keys {
        run(
            "mouse keys",
            mousekeys().and_then(|mut mk| {
                if on {
                    mk.dwFlags |= MKF_MOUSEKEYSON;
                } else {
                    mk.dwFlags &= !MKF_MOUSEKEYSON;
                }
                spi::set_struct(SPI_SETMOUSEKEYS, mk)
            }),
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(errors.join("; "))
    }
}

/// Whether the acceleration curves are Windows' defaults (`None` = not set, which
/// also means default).
pub fn curve_is_default() -> bool {
    let x = reg::get_binary(CURVE_KEY, "SmoothMouseXCurve");
    let y = reg::get_binary(CURVE_KEY, "SmoothMouseYCurve");
    x.is_none_or(|x| x == DEFAULT_X_CURVE) && y.is_none_or(|y| y == DEFAULT_Y_CURVE)
}

/// Restores the default curves. Takes effect after sign-out.
pub fn reset_curve() -> Result<()> {
    reg::set_binary(CURVE_KEY, "SmoothMouseXCurve", &DEFAULT_X_CURVE)?;
    reg::set_binary(CURVE_KEY, "SmoothMouseYCurve", &DEFAULT_Y_CURVE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_scale() {
        assert_eq!(speed_multiplier(10), Some(1.0));
        assert_eq!(speed_multiplier(20), Some(3.5));
        assert_eq!(speed_multiplier(0), None);
        assert_eq!(speed_multiplier(21), None);
    }

    #[test]
    fn toml_fields() {
        let s: MouseSettings =
            toml::from_str("pointer_speed = 10\nenhance_precision = false").unwrap();
        assert_eq!(s.pointer_speed, Some(10));
        assert_eq!(
            toml::to_string(&s).unwrap(),
            "pointer_speed = 10\nenhance_precision = false\n"
        );
        assert!(toml::from_str::<MouseSettings>("pointer_sped = 3").is_err());
    }
}
