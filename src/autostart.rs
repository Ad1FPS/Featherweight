use std::ffi::c_void;
use std::ptr::null_mut;

use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_BINARY,
    RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const VALUE_NAME: &str = "Featherweight";
const OLD_VALUE_NAME: &str = "GPX Tool";
const ERROR_FILE_NOT_FOUND: u32 = 2;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn command_line() -> Result<String, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("could not find the current exe path: {error}"))?;
    Ok(format!("\"{}\" --minimized", exe.display()))
}

pub fn migrate() {
    if reg_get_string(RUN_KEY, OLD_VALUE_NAME).is_none() {
        return;
    }
    let command = match command_line() {
        Ok(command) => command,
        Err(error) => {
            crate::debuglog::record_always(&format!(
                "start-with-windows migration failed: {error}"
            ));
            return;
        }
    };
    if let Err(error) = reg_set_string(RUN_KEY, VALUE_NAME, &command) {
        crate::debuglog::record_always(&format!(
            "start-with-windows migration failed: {error}"
        ));
        return;
    }
    let _ = reg_delete_value(RUN_KEY, OLD_VALUE_NAME);
    let _ = reg_delete_value(APPROVED_KEY, OLD_VALUE_NAME);
    crate::debuglog::record_always(&format!(
        "start-with-windows migrated: \"{OLD_VALUE_NAME}\" -> \"{VALUE_NAME}\" = {command}"
    ));
}

pub fn is_enabled() -> bool {
    if reg_get_string(RUN_KEY, VALUE_NAME).is_none() {
        return false;
    }
    match reg_get_binary(APPROVED_KEY, VALUE_NAME) {
        Some(bytes) if bytes.first().copied() == Some(0x03) => false,
        _ => true,
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let command = command_line()?;
        reg_set_string(RUN_KEY, VALUE_NAME, &command)?;
        let _ = reg_delete_value(APPROVED_KEY, VALUE_NAME);
        Ok(())
    } else {
        reg_delete_value(RUN_KEY, VALUE_NAME)
    }
}

pub fn refresh_if_moved() {
    if !is_enabled() {
        return;
    }
    let Ok(command) = command_line() else {
        return;
    };
    if reg_get_string(RUN_KEY, VALUE_NAME).as_deref() != Some(command.as_str()) {
        let _ = reg_set_string(RUN_KEY, VALUE_NAME, &command);
    }
}

pub fn system_uses_light_theme() -> bool {
    reg_get_dword(PERSONALIZE_KEY, "SystemUsesLightTheme")
        .map(|value| value == 1)
        .unwrap_or(false)
}

pub fn apps_use_light_theme() -> bool {
    reg_get_dword(PERSONALIZE_KEY, "AppsUseLightTheme")
        .map(|value| value == 1)
        .unwrap_or(false)
}

fn reg_get_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let mut units = (size as usize / 2).min(buffer.len());
    while units > 0 && buffer[units - 1] == 0 {
        units -= 1;
    }
    Some(String::from_utf16_lossy(&buffer[..units]))
}

fn reg_get_binary(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut buffer = [0u8; 64];
    let mut size = buffer.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    Some(buffer[..(size as usize).min(buffer.len())].to_vec())
}

fn reg_get_dword(subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut data as *mut u32).cast::<c_void>(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    Some(data)
}

fn reg_set_string(subkey: &str, value: &str, data: &str) -> Result<(), String> {
    let subkey = wide(subkey);
    let value = wide(value);
    let data: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(format!("could not open the Run key (error {status})"));
    }
    let status = unsafe {
        RegSetValueExW(
            key,
            value.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr().cast::<u8>(),
            (data.len() * 2) as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 {
        return Err(format!("could not write the Run value (error {status})"));
    }
    Ok(())
}

fn reg_delete_value(subkey: &str, value: &str) -> Result<(), String> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != 0 {
        return Err(format!("could not open the registry key (error {status})"));
    }
    let status = unsafe { RegDeleteValueW(key, value.as_ptr()) };
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 && status != ERROR_FILE_NOT_FOUND {
        return Err(format!(
            "could not delete the registry value (error {status})"
        ));
    }
    Ok(())
}
