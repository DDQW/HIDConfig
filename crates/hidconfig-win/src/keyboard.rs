//! Windows keyboard settings, including the accessibility hotkeys games trip over and
//! the FilterKeys "custom repeat" trick for finer repeat timing than the slider.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use windows::Win32::UI::{
    Accessibility::{FILTERKEYS, SKF_HOTKEYACTIVE, STICKYKEYS, TOGGLEKEYS},
    WindowsAndMessaging::*,
};

use crate::{
    reg,
    spi::{self, get},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageHotkey {
    AltShift,
    CtrlShift,
    /// The key left of 1 (` / ^ on most layouts).
    Grave,
    None,
}

impl LanguageHotkey {
    fn code(self) -> &'static str {
        match self {
            LanguageHotkey::AltShift => "1",
            LanguageHotkey::CtrlShift => "2",
            LanguageHotkey::None => "3",
            LanguageHotkey::Grave => "4",
        }
    }

    fn from_code(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "1" => LanguageHotkey::AltShift,
            "2" => LanguageHotkey::CtrlShift,
            "3" => LanguageHotkey::None,
            "4" => LanguageHotkey::Grave,
            _ => return None,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardSettings {
    /// 0 (short, ~250 ms) to 3 (long, ~1 s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_delay: Option<u8>,
    /// 0 (~2.5/s) to 31 (~30/s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_rate: Option<u8>,
    /// Text cursor blink interval in ms; 0 = no blinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caret_blink_ms: Option<u32>,

    /// Hidden: exact repeat timing via FilterKeys (overrides delay/rate while on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_repeat: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_repeat_delay_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_repeat_interval_ms: Option<u32>,

    /// Shift ×5 opens Sticky Keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sticky_keys_shortcut: Option<bool>,
    /// Right Shift held 8 s turns on Filter Keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_keys_shortcut: Option<bool>,
    /// Num Lock held 5 s turns on Toggle Keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toggle_keys_shortcut: Option<bool>,
    /// Beep on Caps/Num/Scroll Lock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_key_beep: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_hotkey: Option<LanguageHotkey>,
}

const TOGGLE_KEY: &str = r"Keyboard Layout\Toggle";
const DESKTOP_KEY: &str = r"Control Panel\Desktop";
/// What `SetCaretBlinkTime` / the registry use for "never blink".
const NO_BLINK: u32 = u32::MAX;

fn filterkeys() -> Result<FILTERKEYS> {
    spi::get_struct(SPI_GETFILTERKEYS, FILTERKEYS { cbSize: size_of::<FILTERKEYS>() as u32, ..Default::default() })
}

fn stickykeys() -> Result<STICKYKEYS> {
    spi::get_struct(SPI_GETSTICKYKEYS, STICKYKEYS { cbSize: size_of::<STICKYKEYS>() as u32, ..Default::default() })
}

fn togglekeys() -> Result<TOGGLEKEYS> {
    spi::get_struct(SPI_GETTOGGLEKEYS, TOGGLEKEYS { cbSize: size_of::<TOGGLEKEYS>() as u32, ..Default::default() })
}

fn set_flag(flags: &mut u32, flag: u32, on: bool) {
    if on {
        *flags |= flag;
    } else {
        *flags &= !flag;
    }
}

pub fn read() -> Result<KeyboardSettings> {
    let fk = filterkeys()?;
    let tk = togglekeys()?;
    // SAFETY: plain query.
    let blink = unsafe { GetCaretBlinkTime() };
    Ok(KeyboardSettings {
        repeat_delay: Some(get::<u32>(SPI_GETKEYBOARDDELAY)? as u8),
        repeat_rate: Some(get::<u32>(SPI_GETKEYBOARDSPEED)? as u8),
        caret_blink_ms: Some(if blink == NO_BLINK { 0 } else { blink }),
        custom_repeat: Some(fk.dwFlags & FKF_FILTERKEYSON != 0),
        custom_repeat_delay_ms: Some(fk.iDelayMSec),
        custom_repeat_interval_ms: Some(fk.iRepeatMSec),
        sticky_keys_shortcut: Some(stickykeys()?.dwFlags.contains(SKF_HOTKEYACTIVE)),
        filter_keys_shortcut: Some(fk.dwFlags & FKF_HOTKEYACTIVE != 0),
        toggle_keys_shortcut: Some(tk.dwFlags & TKF_HOTKEYACTIVE != 0),
        lock_key_beep: Some(tk.dwFlags & TKF_TOGGLEKEYSON != 0),
        language_hotkey: reg::get_string(TOGGLE_KEY, "Hotkey").and_then(|s| LanguageHotkey::from_code(&s)),
    })
}

pub fn apply(s: &KeyboardSettings) -> Result<()> {
    let mut errors = Vec::new();
    let mut run = |what: &str, r: Result<()>| {
        if let Err(e) = r {
            errors.push(format!("{what}: {e:#}"));
        }
    };
    if let Some(v) = s.repeat_delay {
        run(
            "repeat delay",
            if v <= 3 { spi::set_ui(SPI_SETKEYBOARDDELAY, v as u32) } else { Err(anyhow::anyhow!("must be 0..=3")) },
        );
    }
    if let Some(v) = s.repeat_rate {
        run(
            "repeat rate",
            if v <= 31 { spi::set_ui(SPI_SETKEYBOARDSPEED, v as u32) } else { Err(anyhow::anyhow!("must be 0..=31")) },
        );
    }
    if let Some(ms) = s.caret_blink_ms {
        run("caret blink", {
            let v = if ms == 0 { NO_BLINK } else { ms };
            // SetCaretBlinkTime is session-only; the registry value makes it stick.
            // SAFETY: plain call.
            let live = unsafe { SetCaretBlinkTime(v) }.map_err(anyhow::Error::from);
            let text = if ms == 0 { "-1".to_string() } else { ms.to_string() };
            live.and(reg::set_string(DESKTOP_KEY, "CursorBlinkRate", &text))
        });
    }
    let fk_touched = s.custom_repeat.is_some()
        || s.custom_repeat_delay_ms.is_some()
        || s.custom_repeat_interval_ms.is_some()
        || s.filter_keys_shortcut.is_some();
    if fk_touched {
        run(
            "filter keys",
            filterkeys().and_then(|mut fk| {
                if let Some(on) = s.custom_repeat {
                    set_flag(&mut fk.dwFlags, FKF_FILTERKEYSON, on);
                    // Repeat only: no slow keys, no bounce keys, no tray icon or clicks.
                    // Turning it off puts the indicator/click flags back to Windows'
                    // defaults (they only matter while FilterKeys is on).
                    if on {
                        fk.iWaitMSec = 0;
                        fk.iBounceMSec = 0;
                    }
                    set_flag(&mut fk.dwFlags, FKF_INDICATOR | FKF_CLICKON, !on);
                }
                if let Some(ms) = s.custom_repeat_delay_ms {
                    fk.iDelayMSec = ms;
                }
                if let Some(ms) = s.custom_repeat_interval_ms {
                    fk.iRepeatMSec = ms;
                }
                if let Some(on) = s.filter_keys_shortcut {
                    set_flag(&mut fk.dwFlags, FKF_HOTKEYACTIVE, on);
                }
                spi::set_struct(SPI_SETFILTERKEYS, fk)
            }),
        );
    }
    if let Some(on) = s.sticky_keys_shortcut {
        run(
            "sticky keys shortcut",
            stickykeys().and_then(|mut sk| {
                set_flag(&mut sk.dwFlags.0, SKF_HOTKEYACTIVE.0, on);
                spi::set_struct(SPI_SETSTICKYKEYS, sk)
            }),
        );
    }
    if s.toggle_keys_shortcut.is_some() || s.lock_key_beep.is_some() {
        run(
            "toggle keys",
            togglekeys().and_then(|mut tk| {
                if let Some(on) = s.toggle_keys_shortcut {
                    set_flag(&mut tk.dwFlags, TKF_HOTKEYACTIVE, on);
                }
                if let Some(on) = s.lock_key_beep {
                    set_flag(&mut tk.dwFlags, TKF_TOGGLEKEYSON, on);
                }
                spi::set_struct(SPI_SETTOGGLEKEYS, tk)
            }),
        );
    }
    if let Some(h) = s.language_hotkey {
        run(
            "language hotkey",
            reg::set_string(TOGGLE_KEY, "Hotkey", h.code())
                .and(reg::set_string(TOGGLE_KEY, "Language Hotkey", h.code()))
                // Makes the input system re-read the Toggle key.
                .and(spi::set_ui(SPI_SETLANGTOGGLE, 0)),
        );
    }
    if errors.is_empty() { Ok(()) } else { bail!(errors.join("; ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_roundtrip() {
        for h in [LanguageHotkey::AltShift, LanguageHotkey::CtrlShift, LanguageHotkey::Grave, LanguageHotkey::None] {
            assert_eq!(LanguageHotkey::from_code(h.code()), Some(h));
        }
    }

    #[test]
    fn flags() {
        let mut f = FKF_HOTKEYACTIVE;
        set_flag(&mut f, FKF_FILTERKEYSON, true);
        assert_eq!(f, FKF_HOTKEYACTIVE | FKF_FILTERKEYSON);
        set_flag(&mut f, FKF_HOTKEYACTIVE, false);
        assert_eq!(f, FKF_FILTERKEYSON);
    }
}
