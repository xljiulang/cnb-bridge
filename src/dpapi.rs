//! Windows DPAPI helpers used to encrypt local secrets at rest.

/// Encrypts bytes for the current Windows user.
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let ok = CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        if ok == 0 {
            return Err(format!(
                "DPAPI 加密失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let result = slice.to_vec();
        LocalFree(output.pbData as _);
        Ok(result)
    }
}

/// Decrypts bytes previously encrypted by [`protect`].
#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        let ok = CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        );
        if ok == 0 {
            return Err(format!(
                "DPAPI 解密失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let result = slice.to_vec();
        LocalFree(output.pbData as _);
        Ok(result)
    }
}

/// DPAPI is only available on Windows.
#[cfg(not(windows))]
pub fn protect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI 仅支持 Windows".to_string())
}

/// DPAPI is only available on Windows.
#[cfg(not(windows))]
pub fn unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI 仅支持 Windows".to_string())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn round_trips_bytes() {
        let plaintext = b"cnb-bridge-secret";
        let encrypted = super::protect(plaintext).unwrap();
        assert_ne!(encrypted, plaintext);
        assert_eq!(super::unprotect(&encrypted).unwrap(), plaintext);
    }
}
