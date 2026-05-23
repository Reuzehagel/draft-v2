// Single-instance gate using a Windows named mutex.
//
// Returns Ok(Some(guard)) if we are the first instance — the guard owns
// the mutex handle and releases it on drop. Returns Ok(None) if another
// instance already holds it.

use anyhow::Result;

#[cfg(windows)]
pub struct InstanceGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub fn acquire() -> Result<Option<InstanceGuard>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Global\\Draft.SingleInstance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(None, true, PCWSTR(name.as_ptr()))?;
        let err = GetLastError();
        if err == ERROR_ALREADY_EXISTS {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(InstanceGuard { handle }))
    }
}

#[cfg(not(windows))]
pub struct InstanceGuard;

#[cfg(not(windows))]
pub fn acquire() -> Result<Option<InstanceGuard>> {
    Ok(Some(InstanceGuard))
}
