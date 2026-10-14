//! Windows bits: single instance, autostart, log file.

use std::{io::Write, path::PathBuf};

use anyhow::Result;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "HIDConfig";

/// Holds the named mutex for the life of the process. `None` if another tray is running.
pub struct SingleInstance(#[allow(dead_code)] windows::Win32::Foundation::HANDLE);

pub fn single_instance() -> Option<SingleInstance> {
    use windows::{
        Win32::{
            Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
            System::Threading::CreateMutexW,
        },
        core::w,
    };
    // SAFETY: plain Win32 call with a static name; the handle is kept until exit.
    let handle = unsafe { CreateMutexW(None, true, w!("Local\\HIDConfig-tray")) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return None;
    }
    Some(SingleInstance(handle))
}

fn exe_command() -> Result<String> {
    Ok(format!("\"{}\"", std::env::current_exe()?.display()))
}

pub fn autostart_enabled() -> bool {
    windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|k| k.get_string(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(on: bool) -> Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if on {
        key.set_string(RUN_VALUE, exe_command()?)?;
    } else if key.get_string(RUN_VALUE).is_ok() {
        key.remove_value(RUN_VALUE)?;
    }
    Ok(())
}

fn log_path() -> Option<PathBuf> {
    Some(hidconfig_settings::path().ok()?.with_file_name("tray.log"))
}

/// Starts a fresh log if the old one passed 1 MB.
pub fn init_log() {
    if let Some(p) = log_path()
        && std::fs::metadata(&p).is_ok_and(|m| m.len() > 1 << 20)
    {
        let _ = std::fs::rename(&p, p.with_extension("log.old"));
    }
}

pub fn log(msg: &str) {
    let Some(p) = log_path() else { return };
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

/// Opens the config in Notepad, creating an empty one first if needed.
pub fn open_config() -> Result<()> {
    let p = hidconfig_settings::path()?;
    if !p.exists() {
        hidconfig_settings::Config::default().save()?;
    }
    std::process::Command::new("notepad.exe").arg(p).spawn()?;
    Ok(())
}
