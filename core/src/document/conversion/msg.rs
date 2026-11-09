//! Read Outlook MSG through Windows' read-only compound-file API.
//! No Outlook instance, active content, attachment paths or runtime dependency.
use super::Result;
use serde_json::Value;

pub(super) fn available() -> bool {
    cfg!(windows)
}

#[cfg(not(windows))]
pub(crate) fn read(_: &[u8]) -> Result<Value> {
    Err("MAIL_FORMAT_UNSUPPORTED")
}

#[cfg(windows)]
pub(crate) fn read(bytes: &[u8]) -> Result<Value> {
    windows::read(bytes)
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::catalog::mail::MAX_MESSAGE;
    use mail_parser::{decoders::html::html_to_text, MessageParser};
    use serde_json::json;
    use std::{
        ffi::c_void,
        ptr::{self, NonNull},
    };

    const INVALID: &str = "MAIL_MSG_INVALID";
    const LIMIT: &str = "MAIL_FILE_LIMIT";
    const READ_EXCLUSIVE: u32 = 0x10;
    const MISSING: i32 = 0x80030002u32 as i32;
    const MAGIC: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1";

    #[repr(C)]
    struct Interface {
        vtable: *const *const c_void,
    }
    type ComPtr = *mut Interface;
    #[link(name = "ole32")]
    extern "system" {
        fn CreateILockBytesOnHGlobal(
            handle: *mut c_void,
            delete_on_release: i32,
            out: *mut ComPtr,
        ) -> i32;
        fn StgOpenStorageOnILockBytes(
            bytes: ComPtr,
            priority: ComPtr,
            mode: u32,
            exclude: *const c_void,
            reserved: u32,
            out: *mut ComPtr,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(handle: *mut c_void) -> *mut u8;
        fn GlobalUnlock(handle: *mut c_void) -> i32;
        fn GlobalFree(handle: *mut c_void) -> *mut c_void;
        fn MultiByteToWideChar(
            page: u32,
            flags: u32,
            src: *const u8,
            src_len: i32,
            dst: *mut u16,
            dst_len: i32,
        ) -> i32;
    }

    struct Com(NonNull<Interface>);
    impl Com {
        fn receive(call: impl FnOnce(*mut ComPtr) -> i32) -> Result<Option<Self>> {
            let mut value = ptr::null_mut();
            match call(&mut value) {
                MISSING => Ok(None),
                code if code >= 0 => NonNull::new(value).map(Self).map(Some).ok_or(INVALID),
                _ => Err(INVALID),
            }
        }
        fn get(&self) -> ComPtr {
            self.0.as_ptr()
        }
        // Slots are fixed by the IUnknown, IStorage and IStream COM ABIs.
        fn slot(&self, index: usize) -> *const c_void {
            unsafe { *(*self.get()).vtable.add(index) }
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            let release: unsafe extern "system" fn(ComPtr) -> u32 =
                unsafe { std::mem::transmute(self.slot(2)) };
            unsafe { release(self.get()) };
        }
    }

    struct Global(*mut c_void);
    impl Drop for Global {
        fn drop(&mut self) {
            unsafe { GlobalFree(self.0) };
        }
    }

    fn lock_bytes(bytes: &[u8]) -> Result<Com> {
        let handle = unsafe { GlobalAlloc(0x42, bytes.len()) }; // movable, zero-init including rounding
        if handle.is_null() {
            return Err(LIMIT);
        }
        let owner = Global(handle);
        let target = unsafe { GlobalLock(handle) };
        if target.is_null() {
            return Err(INVALID);
        }
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            GlobalUnlock(handle);
        }
        let lock = Com::receive(|out| unsafe { CreateILockBytesOnHGlobal(handle, 1, out) })?
            .ok_or(INVALID)?;
        std::mem::forget(owner);
        Ok(lock)
    }

    struct Storage(Com);
    impl Storage {
        fn open(lock: &Com) -> Result<Self> {
            Com::receive(|out| unsafe {
                StgOpenStorageOnILockBytes(
                    lock.get(),
                    ptr::null_mut(),
                    READ_EXCLUSIVE,
                    ptr::null(),
                    0,
                    out,
                )
            })?
            .map(Self)
            .ok_or(INVALID)
        }
        fn storage(&self, name: &str) -> Result<Option<Self>> {
            let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let open: unsafe extern "system" fn(
                ComPtr,
                *const u16,
                ComPtr,
                u32,
                *const c_void,
                u32,
                *mut ComPtr,
            ) -> i32 = unsafe { std::mem::transmute(self.0.slot(6)) };
            Ok(Com::receive(|out| unsafe {
                open(
                    self.0.get(),
                    name.as_ptr(),
                    ptr::null_mut(),
                    READ_EXCLUSIVE,
                    ptr::null(),
                    0,
                    out,
                )
            })?
            .map(Self))
        }
        fn stream(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>> {
            let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let open: unsafe extern "system" fn(
                ComPtr,
                *const u16,
                *mut c_void,
                u32,
                u32,
                *mut ComPtr,
            ) -> i32 = unsafe { std::mem::transmute(self.0.slot(4)) };
            let Some(stream) = Com::receive(|out| unsafe {
                open(
                    self.0.get(),
                    name.as_ptr(),
                    ptr::null_mut(),
                    READ_EXCLUSIVE,
                    0,
                    out,
                )
            })?
            else {
                return Ok(None);
            };
            let read: unsafe extern "system" fn(ComPtr, *mut u8, u32, *mut u32) -> i32 =
                unsafe { std::mem::transmute(stream.slot(3)) };
            let mut result = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let mut count = 0;
                if unsafe {
                    read(
                        stream.get(),
                        buffer.as_mut_ptr(),
                        buffer.len() as u32,
                        &mut count,
                    )
                } < 0
                {
                    return Err(INVALID);
                }
                let count = count as usize;
                if result.len() + count > limit {
                    return Err(LIMIT);
                }
                result.extend_from_slice(buffer.get(..count).ok_or(INVALID)?);
                if count < buffer.len() {
                    return Ok(Some(result));
                }
            }
        }
        fn text(&self, property: &str, page: u32, limit: usize) -> Result<String> {
            if let Some(raw) = self.stream(&format!("__substg1.0_{property}001F"), limit)? {
                if raw.len() % 2 != 0 {
                    return Err(INVALID);
                }
                let mut wide: Vec<_> = raw
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect();
                while wide.last() == Some(&0) {
                    wide.pop();
                }
                return String::from_utf16(&wide).map_err(|_| INVALID);
            }
            self.stream(&format!("__substg1.0_{property}001E"), limit)?
                .map_or_else(|| Ok(String::new()), |raw| decode(&raw, page))
        }
    }

    fn decode(bytes: &[u8], page: u32) -> Result<String> {
        let bytes = &bytes[..bytes.iter().rposition(|&v| v != 0).map_or(0, |n| n + 1)];
        if bytes.is_empty() {
            return Ok(String::new());
        }
        if let Ok(value) = std::str::from_utf8(bytes) {
            if page == 65001 || bytes.is_ascii() {
                return Ok(value.to_owned());
            }
        }
        let size = bytes.len().try_into().map_err(|_| LIMIT)?;
        let n = unsafe { MultiByteToWideChar(page, 8, bytes.as_ptr(), size, ptr::null_mut(), 0) };
        if n <= 0 {
            return Err("MAIL_MSG_ENCODING_UNSUPPORTED");
        }
        let mut out = vec![0; n as usize];
        if unsafe { MultiByteToWideChar(page, 8, bytes.as_ptr(), size, out.as_mut_ptr(), n) } != n {
            return Err(INVALID);
        }
        String::from_utf16(&out).map_err(|_| INVALID)
    }
    fn property(data: &[u8], tag: u32) -> Option<&[u8]> {
        data.get(32..)?
            .chunks_exact(16)
            .find(|entry| entry[..4] == tag.to_le_bytes())
            .map(|entry| &entry[8..16])
    }
    fn date(properties: &[u8]) -> String {
        [0x00390040, 0x0e060040]
            .into_iter()
            .find_map(|tag| {
                let ticks = i64::from_le_bytes(property(properties, tag)?.try_into().ok()?);
                let seconds = ticks.checked_div(10_000_000)?.checked_sub(11_644_473_600)?;
                Some(chrono::DateTime::from_timestamp(seconds, 0)?.to_rfc3339())
            })
            .unwrap_or_default()
    }
    fn prop_u32(data: &[u8], tag: u32) -> Option<u32> {
        Some(u32::from_le_bytes(
            property(data, tag)?[..4].try_into().ok()?,
        ))
    }

    pub(super) fn read(bytes: &[u8]) -> Result<Value> {
        if bytes.len() > MAX_MESSAGE {
            return Err(LIMIT);
        }
        if !bytes.starts_with(MAGIC) {
            return Err(INVALID);
        }
        let lock = lock_bytes(bytes)?;
        let root = Storage::open(&lock)?;
        let props = root
            .stream("__properties_version1.0", 1024 * 1024)?
            .ok_or(INVALID)?;
        if props.len() < 32 || (props.len() - 32) % 16 != 0 {
            return Err(INVALID);
        }
        let page = prop_u32(&props, 0x3ffd0003)
            .filter(|v| *v != 0)
            .unwrap_or(1252);
        let html_page = prop_u32(&props, 0x3fde0003)
            .filter(|v| *v != 0)
            .unwrap_or(page);
        let field = |id: &str| root.text(id, page, 64 * 1024);
        let headers = field("007D")?;
        let headers = if headers.is_empty() {
            None
        } else {
            MessageParser::default().parse(headers.as_bytes())
        };
        let header = |id: &'static str| {
            headers
                .as_ref()
                .and_then(|m| m.header(id))
                .and_then(|v| v.as_text())
                .unwrap_or_default()
                .to_owned()
        };
        let prefer = |a: String, b: String| if a.trim().is_empty() { b } else { a };
        let subject = prefer(field("0037")?, header("Subject"));
        let from = prefer(field("0C1A")?, prefer(field("5D01")?, field("0C1F")?));
        let to = prefer(field("0E04")?, header("To"));
        let msg_id = prefer(field("1035")?, header("Message-ID"));
        let thread = prefer(field("1042")?, header("In-Reply-To"));
        let date = prefer(
            date(&props),
            headers
                .as_ref()
                .and_then(|m| m.date())
                .map(|v| v.to_rfc3339())
                .unwrap_or_default(),
        );
        let mut body = root.text("1000", page, MAX_MESSAGE)?;
        if body.trim().is_empty() {
            if let Some(html) = root.stream("__substg1.0_10130102", MAX_MESSAGE)? {
                body = html_to_text(&decode(&html, html_page)?);
            } else {
                body = html_to_text(&root.text("1013", html_page, MAX_MESSAGE)?);
            }
        }
        let subject = super::super::mail::label(&subject, 600);
        let from = super::super::mail::label(&from, 600);
        let to = super::super::mail::label(&to, 1200);
        let mut text = format!("Тема: {subject}\nОт: {from}\nКому: {to}\nДата: {date}\n\n");
        text.push_str(body.trim());
        text.push_str("\n\n");
        let count = u32::from_le_bytes(props[20..24].try_into().map_err(|_| INVALID)?);
        let next = u32::from_le_bytes(props[12..16].try_into().map_err(|_| INVALID)?);
        if count > 100 || next > 4096 {
            return Err("MAIL_PART_LIMIT");
        }
        let mut attachments = Vec::new();
        let mut remaining = MAX_MESSAGE;
        for i in 0..next {
            if attachments.len() == count as usize {
                break;
            }
            let Some(item) = root.storage(&format!("__attach_version1.0_#{i:08X}"))? else {
                continue;
            };
            let name = prefer(
                item.text("3707", page, 64 * 1024)?,
                item.text("3704", page, 64 * 1024)?,
            );
            let name = super::super::mail::label(&name, 250);
            let data = item.stream("__substg1.0_37010102", remaining)?;
            remaining -= data.as_ref().map_or(0, Vec::len);
            super::super::mail::append_attachment(
                &mut text,
                &mut attachments,
                &name,
                data.as_deref(),
                false,
                false,
            );
        }
        if attachments.len() != count as usize {
            return Err(INVALID);
        }
        if body.trim().is_empty() && subject.is_empty() && from.is_empty() {
            return Err(INVALID);
        }
        Ok(
            json!({"kind":"mail","text":text,"metadata":{"extractor":"les.mail-msg.v1","attachments":attachments,
            "subject":subject,"sender":from,"recipients":to,"date":date,"message_id":msg_id,"thread_id":thread}}),
        )
    }
}
