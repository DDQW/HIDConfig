//! Windows input (and later audio) settings that HIDConfig can manage.
//!
//! Each area has a settings struct whose fields are all optional, `read()` for the
//! current system values, and `apply()` that writes only the fields that are set.

pub mod keyboard;
pub mod mouse;
mod reg;
mod spi;

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use keyboard::KeyboardSettings;
pub use mouse::MouseSettings;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowsSettings {
    pub mouse: MouseSettings,
    pub keyboard: KeyboardSettings,
}

impl WindowsSettings {
    pub fn is_empty(&self) -> bool {
        *self == WindowsSettings::default()
    }
}

pub fn read() -> Result<WindowsSettings> {
    Ok(WindowsSettings { mouse: mouse::read()?, keyboard: keyboard::read()? })
}

/// Applies `s`. The first time anything is applied, the current values of every
/// managed setting are saved to `backup` so they can be restored later.
pub fn apply(s: &WindowsSettings, backup: &Path) -> Result<()> {
    if s.is_empty() {
        return Ok(());
    }
    if !backup.exists() {
        save_backup(backup).context("saving backup of current Windows settings")?;
    }
    let mut errors = Vec::new();
    if let Err(e) = mouse::apply(&s.mouse) {
        errors.push(format!("mouse: {e:#}"));
    }
    if let Err(e) = keyboard::apply(&s.keyboard) {
        errors.push(format!("keyboard: {e:#}"));
    }
    if errors.is_empty() { Ok(()) } else { anyhow::bail!(errors.join("; ")) }
}

fn save_backup(path: &Path) -> Result<()> {
    let current = read()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = format!(
        "# Windows settings as they were before HIDConfig first changed them.\n\
         # `hidconfigctl win restore` puts these back.\n{}",
        toml::to_string_pretty(&current)?
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
    Ok(saved)
}
