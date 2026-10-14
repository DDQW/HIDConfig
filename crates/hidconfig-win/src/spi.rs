//! Thin wrappers over `SystemParametersInfoW` and friends.
//!
//! SPI is inconsistent about where a value goes: some setters take it in `uiParam`,
//! others in `pvParam` (as the pointer value itself), and getters write through
//! `pvParam`. The helpers are named after that.

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
        SYSTEM_PARAMETERS_INFO_ACTION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageTimeoutW,
        SystemParametersInfoW, WM_SETTINGCHANGE,
    },
};

const PERSIST: SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS =
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(SPIF_UPDATEINIFILE.0 | SPIF_SENDCHANGE.0);

/// Getter that writes a `T` through `pvParam`.
pub fn get<T: Default>(action: SYSTEM_PARAMETERS_INFO_ACTION) -> Result<T> {
    let mut v = T::default();
    // SAFETY: pvParam points at a live `T` of the size the action expects.
    unsafe {
        SystemParametersInfoW(
            action,
            0,
            Some(&mut v as *mut T as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .with_context(|| format!("SPI get {:#x}", action.0))?;
    Ok(v)
}

/// Getter for structs with a leading `cbSize` passed in `uiParam`.
pub fn get_struct<T>(action: SYSTEM_PARAMETERS_INFO_ACTION, mut v: T) -> Result<T> {
    // SAFETY: `v` is a properly sized struct with cbSize set by the caller.
    unsafe {
        SystemParametersInfoW(
            action,
            size_of::<T>() as u32,
            Some(&mut v as *mut T as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .with_context(|| format!("SPI get {:#x}", action.0))?;
    Ok(v)
}

pub fn set_struct<T>(action: SYSTEM_PARAMETERS_INFO_ACTION, mut v: T) -> Result<()> {
    // SAFETY: as above; the struct outlives the call.
    unsafe {
        SystemParametersInfoW(
            action,
            size_of::<T>() as u32,
            Some(&mut v as *mut T as *mut c_void),
            PERSIST,
        )
    }
    .with_context(|| format!("SPI set {:#x}", action.0))
}

/// Setter taking the value in `uiParam`.
pub fn set_ui(action: SYSTEM_PARAMETERS_INFO_ACTION, value: u32) -> Result<()> {
    // SAFETY: no pointers involved.
    unsafe { SystemParametersInfoW(action, value, None, PERSIST) }
        .with_context(|| format!("SPI set {:#x} = {value}", action.0))
}

/// Setter taking the value as the `pvParam` pointer value itself.
pub fn set_pv(action: SYSTEM_PARAMETERS_INFO_ACTION, value: usize) -> Result<()> {
    // SAFETY: the "pointer" is a plain integer the API reads as a value.
    unsafe { SystemParametersInfoW(action, 0, Some(value as *mut c_void), PERSIST) }
        .with_context(|| format!("SPI set {:#x} = {value}", action.0))
}

/// Setter writing an array/struct through `pvParam` with `uiParam = 0`.
pub fn set_ptr<T>(action: SYSTEM_PARAMETERS_INFO_ACTION, mut v: T) -> Result<()> {
    // SAFETY: pvParam points at a live `T` for the duration of the call.
    unsafe { SystemParametersInfoW(action, 0, Some(&mut v as *mut T as *mut c_void), PERSIST) }
        .with_context(|| format!("SPI set {:#x}", action.0))
}

/// Tells running apps that a registry-only setting changed.
pub fn broadcast_setting_change(area: &str) {
    let wide: Vec<u16> = area.encode_utf16().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated buffer alive for the (bounded) call.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(wide.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        );
    }
}
