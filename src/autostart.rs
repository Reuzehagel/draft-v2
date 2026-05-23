// Per-user autostart via HKCU\Software\Microsoft\Windows\CurrentVersion\Run.
// No admin needed, no Task Scheduler quirks.

use anyhow::{anyhow, Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

pub fn is_enabled() -> bool {
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            KEY_READ,
            &mut hkey,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let exists = RegQueryValueExW(hkey, w!("Draft"), None, None, None, None) == ERROR_SUCCESS;
        let _ = RegCloseKey(hkey);
        exists
    }
}

pub fn set_enabled(enable: bool) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let mut quoted: Vec<u16> = "\"".encode_utf16().collect();
    quoted.extend(exe.to_string_lossy().encode_utf16());
    quoted.extend("\"\0".encode_utf16());

    unsafe {
        let mut hkey = HKEY::default();
        let open = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            KEY_READ | KEY_WRITE,
            &mut hkey,
        );
        if open != ERROR_SUCCESS {
            return Err(anyhow!("RegOpenKeyExW failed: {:?}", open));
        }

        let result = if enable {
            let bytes: &[u8] =
                std::slice::from_raw_parts(quoted.as_ptr() as *const u8, quoted.len() * 2);
            RegSetValueExW(hkey, w!("Draft"), 0, REG_SZ, Some(bytes))
        } else {
            let r = RegDeleteValueW(hkey, w!("Draft"));
            if r == ERROR_FILE_NOT_FOUND {
                ERROR_SUCCESS
            } else {
                r
            }
        };
        let _ = RegCloseKey(hkey);
        if result != ERROR_SUCCESS {
            return Err(anyhow!("registry write failed: {:?}", result));
        }
    }
    Ok(())
}
