use anyhow::{Context, Result};
use std::path::PathBuf;

#[cfg(windows)]
pub fn is_elevated() -> Result<bool> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            anyhow::bail!("OpenProcessToken failed: {}", GetLastError());
        }
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let error = if ok == 0 { Some(GetLastError()) } else { None };
        CloseHandle(token);
        if let Some(code) = error {
            anyhow::bail!("GetTokenInformation failed: {code}");
        }
        Ok(elevation.TokenIsElevated != 0)
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> Result<bool> {
    anyhow::bail!("Windows host is supported only on Windows")
}

#[cfg(windows)]
pub fn choose_shell() -> Result<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let shell = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    anyhow::ensure!(
        shell.is_file(),
        "Windows PowerShell was not found at {}",
        shell.display()
    );
    Ok(shell)
}

#[cfg(not(windows))]
pub fn choose_shell() -> Result<PathBuf> {
    anyhow::bail!("Windows PowerShell is supported only on Windows")
}

#[cfg(windows)]
pub fn effective_user() -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return "unknown".into();
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut words = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            words.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return "unknown".into();
        }
        let user = &*(words.as_ptr() as *const TOKEN_USER);
        let mut name_len = 0u32;
        let mut domain_len = 0u32;
        let mut sid_type = 0i32;
        LookupAccountSidW(
            std::ptr::null(),
            user.User.Sid,
            std::ptr::null_mut(),
            &mut name_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut sid_type,
        );
        let mut name = vec![0u16; name_len as usize];
        let mut domain = vec![0u16; domain_len as usize];
        if LookupAccountSidW(
            std::ptr::null(),
            user.User.Sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut sid_type,
        ) == 0
        {
            return "unknown".into();
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
        if domain.is_empty() {
            name
        } else {
            format!(r"{}\{}", domain, name)
        }
    }
}
#[cfg(not(windows))]
pub fn effective_user() -> String {
    "unsupported".into()
}

pub fn ensure_absolute_directory(path: &str) -> Result<PathBuf> {
    let p = PathBuf::from(path);
    anyhow::ensure!(p.is_absolute(), "cwd must be an absolute path");
    let canonical = p
        .canonicalize()
        .with_context(|| format!("invalid cwd: {path}"))?;
    anyhow::ensure!(canonical.is_dir(), "cwd is not a directory");
    Ok(canonical)
}
