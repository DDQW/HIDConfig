//! Plug and Play helpers.

use windows::{
    Win32::Devices::DeviceAndDriverInstallation::{
        CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_SUCCESS,
    },
    core::PCWSTR,
};

/// Whether a device instance is currently present (connected and enumerated).
pub fn is_present(instance_id: &str) -> bool {
    let wide: Vec<u16> = instance_id.encode_utf16().chain([0]).collect();
    let mut node = 0u32;
    // SAFETY: NUL-terminated id alive for the call; `node` is a plain out value.
    unsafe {
        CM_Locate_DevNodeW(&mut node, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS
    }
}
