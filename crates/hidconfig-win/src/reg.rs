//! HKCU registry helpers. Reads return `None` when the key or value is missing.

use anyhow::{Context, Result};
use windows_registry::{CURRENT_USER, Type};

pub fn get_dword(key: &str, name: &str) -> Option<u32> {
    CURRENT_USER.open(key).ok()?.get_u32(name).ok()
}

pub fn get_string(key: &str, name: &str) -> Option<String> {
    CURRENT_USER.open(key).ok()?.get_string(name).ok()
}

pub fn get_binary(key: &str, name: &str) -> Option<Vec<u8>> {
    CURRENT_USER.open(key).ok()?.get_bytes(name).ok()
}

pub fn set_dword(key: &str, name: &str, v: u32) -> Result<()> {
    CURRENT_USER
        .create(key)?
        .set_u32(name, v)
        .with_context(|| format!(r"HKCU\{key}\{name}"))
}

pub fn set_string(key: &str, name: &str, v: &str) -> Result<()> {
    CURRENT_USER
        .create(key)?
        .set_string(name, v)
        .with_context(|| format!(r"HKCU\{key}\{name}"))
}

pub fn set_binary(key: &str, name: &str, v: &[u8]) -> Result<()> {
    CURRENT_USER
        .create(key)?
        .set_bytes(name, Type::Bytes, v)
        .with_context(|| format!(r"HKCU\{key}\{name}"))
}
