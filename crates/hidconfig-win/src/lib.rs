//! Windows input (and later audio) settings that HIDConfig can manage.
//!
//! Each area has a settings struct whose fields are all optional, `read()` for the
//! current system values, and `apply()` that writes only the fields that are set.

pub mod audio;
mod devices;
pub mod display;
pub mod gamepad;
pub mod keyboard;
pub mod mouse;
mod reg;
mod spi;

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use audio::AudioSettings;
pub use keyboard::KeyboardSettings;
pub use mouse::MouseSettings;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowsSettings {
    pub mouse: MouseSettings,
    pub keyboard: KeyboardSettings,
    pub audio: AudioSettings,
    pub gaming: gamepad::GamingSettings,
}

impl WindowsSettings {
    pub fn is_empty(&self) -> bool {
        *self == WindowsSettings::default()
    }

    /// Sets one field by path, e.g. `("mouse.pointer_speed", "11")`. The value is
    /// parsed as TOML (numbers, `true`/`false`), falling back to a string.
    pub fn set_field(&mut self, path: &str, value: &str) -> Result<()> {
        let (section, field) = path.split_once('.').context("expected <section>.<field>")?;
        let value = value.trim();
        let parsed: toml::Value = toml::from_str::<toml::Table>(&format!("v = {value}"))
            .ok()
            .and_then(|mut t| t.remove("v"))
            .unwrap_or_else(|| toml::Value::String(value.to_string()));
        let mut table = toml::Table::try_from(&*self)?;
        let sec = table
            .entry(section)
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .context("not a section")?;
        sec.insert(field.to_string(), parsed);
        *self = table
            .try_into()
            .with_context(|| format!("invalid {path} = {value}"))?;
        Ok(())
    }

    /// Fields set in `other` overwrite ours.
    pub fn merge(&mut self, other: &WindowsSettings) -> Result<()> {
        let mut table = toml::Table::try_from(&*self)?;
        for (sec, v) in toml::Table::try_from(other)? {
            if let (Some(toml::Value::Table(dst)), toml::Value::Table(src)) =
                (table.get_mut(&sec), v)
            {
                dst.extend(src);
            }
        }
        *self = table.try_into()?;
        Ok(())
    }
}

pub fn read() -> Result<WindowsSettings> {
    Ok(WindowsSettings {
        gaming: gamepad::read()?,
        mouse: mouse::read()?,
        keyboard: keyboard::read()?,
        audio: audio::read()?,
    })
}

/// Applies `s`. The first time anything is applied, the current values of every
/// managed setting are saved to `backup` so they can be restored later.
pub fn apply(s: &WindowsSettings, backup: &Path) -> Result<()> {
    if s.is_empty() {
        return Ok(());
    }
    update_backup(backup, s).context("saving backup of current Windows settings")?;
    let mut errors = Vec::new();
    if let Err(e) = mouse::apply(&s.mouse) {
        errors.push(format!("mouse: {e:#}"));
    }
    if let Err(e) = keyboard::apply(&s.keyboard) {
        errors.push(format!("keyboard: {e:#}"));
    }
    if let Err(e) = audio::apply(&s.audio) {
        errors.push(format!("audio: {e:#}"));
    }
    if let Err(e) = gamepad::apply(&s.gaming) {
        errors.push(format!("gaming: {e:#}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("; "))
    }
}

/// Records the current values of every section `s` touches that the backup doesn't
/// cover yet (sections added in later versions get backed up on first use too).
fn update_backup(path: &Path, s: &WindowsSettings) -> Result<()> {
    let mut saved: WindowsSettings = match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)?,
        Err(_) => WindowsSettings::default(),
    };
    let before = saved.clone();
    if saved.mouse == MouseSettings::default() && s.mouse != MouseSettings::default() {
        saved.mouse = mouse::read()?;
    }
    if saved.keyboard == KeyboardSettings::default() && s.keyboard != KeyboardSettings::default() {
        saved.keyboard = keyboard::read()?;
    }
    if saved.audio == AudioSettings::default() && s.audio != AudioSettings::default() {
        saved.audio = audio::read()?;
    }
    if saved.gaming == Default::default() && s.gaming != Default::default() {
        saved.gaming = gamepad::read()?;
    }
    if saved == before && path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = format!(
        "# Windows settings as they were before HIDConfig first changed them.\n\
         # `hidconfigctl win restore` puts these back.\n{}",
        toml::to_string_pretty(&saved)?
    );
    std::fs::write(path, text)?;
    Ok(())
}

/// Puts the backed-up values back.
pub fn restore(backup: &Path) -> Result<WindowsSettings> {
    let text = std::fs::read_to_string(backup)
        .with_context(|| format!("no backup at {}", backup.display()))?;
    let saved: WindowsSettings = toml::from_str(&text)?;
    mouse::apply(&saved.mouse)?;
    keyboard::apply(&saved.keyboard)?;
    audio::apply(&saved.audio)?;
    gamepad::apply(&saved.gaming)?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_field_parses_types() {
        let mut s = WindowsSettings::default();
        s.set_field("mouse.pointer_speed", "11").unwrap();
        s.set_field("mouse.enhance_precision", "true").unwrap();
        s.set_field("keyboard.language_hotkey", "ctrl-shift")
            .unwrap();
        assert_eq!(s.mouse.pointer_speed, Some(11));
        assert_eq!(s.mouse.enhance_precision, Some(true));
        assert_eq!(
            s.keyboard.language_hotkey,
            Some(keyboard::LanguageHotkey::CtrlShift)
        );
        assert!(s.set_field("mouse.pointer_sped", "1").is_err());
        assert!(s.set_field("mouse.pointer_speed", "fast").is_err());
        assert_eq!(
            s.mouse.pointer_speed,
            Some(11),
            "failed set leaves the value alone"
        );
    }
}
