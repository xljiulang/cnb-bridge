//! Desktop lifecycle coordination used by the Windows installer.

pub const APP_MUTEX_NAME: &str = r"Local\cool.cnb.bridge.desktop";
pub const AUTOSTART_ARG: &str = "--autostart";
pub const SHUTDOWN_FOR_UPDATE_ARG: &str = "--shutdown-for-update";

/// Returns true only when the autostart argument is present.
pub fn is_autostart<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    has_argument(args, AUTOSTART_ARG)
}

/// Returns true only when the installer shutdown argument is present.
pub fn is_shutdown_for_update<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    has_argument(args, SHUTDOWN_FOR_UPDATE_ARG)
}

fn has_argument<I, S>(args: I, expected: &str) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .skip(1)
        .any(|argument| argument.as_ref() == expected)
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    use super::APP_MUTEX_NAME;

    /// Named mutex held for the lifetime of the desktop process.
    pub struct AppLifecycleMutex {
        handle: HANDLE,
    }

    unsafe impl Send for AppLifecycleMutex {}
    unsafe impl Sync for AppLifecycleMutex {}

    impl AppLifecycleMutex {
        pub fn acquire() -> Result<Self, String> {
            let name = OsStr::new(APP_MUTEX_NAME)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
            if handle.is_null() {
                return Err(format!(
                    "创建应用生命周期互斥量失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                let _ = unsafe { CloseHandle(handle) };
                return Err(format!("应用生命周期互斥量 {} 已存在", APP_MUTEX_NAME));
            }
            Ok(Self { handle })
        }
    }

    impl Drop for AppLifecycleMutex {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(not(windows))]
mod platform {
    pub struct AppLifecycleMutex;

    impl AppLifecycleMutex {
        pub fn acquire() -> Result<Self, String> {
            Ok(Self)
        }
    }
}

pub use platform::AppLifecycleMutex;
