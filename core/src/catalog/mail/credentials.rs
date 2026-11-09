//! Same per-account credential IDs and UTF-8 values as the retained collector.
type Result<T> = std::result::Result<T, &'static str>;

#[cfg(windows)]
mod platform {
    use super::Result;
    use std::{ffi::c_void, ptr};

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target: *const u16,
        comment: *const u16,
        last_written: [u32; 2],
        size: u32,
        blob: *const u8,
        persist: u32,
        count: u32,
        attrs: *const c_void,
        alias: *const u16,
        username: *const u16,
    }
    #[link(name = "Advapi32")]
    extern "system" {
        fn CredReadW(
            target: *const u16,
            kind: u32,
            flags: u32,
            result: *mut *mut Credential,
        ) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
    fn target(id: &str) -> Vec<u16> {
        wide(&format!("me.ovc.les-light.mail.{id}"))
    }
    fn missing() -> bool {
        std::io::Error::last_os_error().raw_os_error() == Some(1168)
    }
    struct Owned(*mut Credential);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe { CredFree(self.0.cast()) };
        }
    }
    pub fn get(id: &str) -> Result<Option<String>> {
        let mut credential = ptr::null_mut();
        if unsafe { CredReadW(target(id).as_ptr(), 1, 0, &mut credential) } == 0 {
            return if missing() {
                Ok(None)
            } else {
                Err("MAIL_SECRET_STORE_UNAVAILABLE")
            };
        }
        if credential.is_null() {
            return Err("MAIL_SECRET_STORE_UNAVAILABLE");
        }
        let guard = Owned(credential);
        let value = unsafe { &*guard.0 };
        if value.size == 0 {
            return Ok(None);
        }
        if value.size > 2560 || value.blob.is_null() {
            return Err("MAIL_SECRET_INVALID");
        }
        let bytes = unsafe { std::slice::from_raw_parts(value.blob, value.size as usize) };
        String::from_utf8(bytes.to_vec())
            .map(Some)
            .map_err(|_| "MAIL_SECRET_INVALID")
    }
    pub fn set(id: &str, secret: &str) -> Result<()> {
        let name = target(id);
        if secret.is_empty() {
            return if unsafe { CredDeleteW(name.as_ptr(), 1, 0) } != 0 || missing() {
                Ok(())
            } else {
                Err("MAIL_SECRET_STORE_UNAVAILABLE")
            };
        }
        let username = wide("LES mail account");
        let value = Credential {
            flags: 0,
            kind: 1,
            target: name.as_ptr(),
            comment: ptr::null(),
            last_written: [0; 2],
            size: secret.len() as u32,
            blob: secret.as_ptr(),
            persist: 2,
            count: 0,
            attrs: ptr::null(),
            alias: ptr::null(),
            username: username.as_ptr(),
        };
        if unsafe { CredWriteW(&value, 0) } != 0 {
            Ok(())
        } else {
            Err("MAIL_SECRET_STORE_UNAVAILABLE")
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Result;
    pub fn get(_: &str) -> Result<Option<String>> {
        Err("MAIL_SECRET_STORE_UNAVAILABLE")
    }
    pub fn set(_: &str, _: &str) -> Result<()> {
        Err("MAIL_SECRET_STORE_UNAVAILABLE")
    }
}

pub(super) use platform::{get, set};
