//! MS-DOC piece table, implemented from Microsoft's Open Specifications.
//! The Windows compound-storage reader does not require Word or execute macros.
use std::path::Path;

const LIMIT: usize = 100 * 1024 * 1024;
fn u16_at(data: &[u8], at: usize) -> Result<u16, &'static str> {
    Ok(u16::from_le_bytes(
        data.get(at..at + 2)
            .ok_or("DOC_TRUNCATED")?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32, &'static str> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .ok_or("DOC_TRUNCATED")?
            .try_into()
            .unwrap(),
    ))
}

fn table_info(word: &[u8]) -> Result<(bool, usize, usize), &'static str> {
    if u16_at(word, 0)? != 0xa5ec || u16_at(word, 2)? < 0xc1 {
        return Err("DOC_VERSION_UNSUPPORTED");
    }
    let flags = u16_at(word, 10)?;
    if flags & 0x8100 != 0 {
        return Err("DOC_ENCRYPTED");
    }
    let csw = usize::from(u16_at(word, 32)?);
    let lw = 34 + csw * 2;
    let clw = usize::from(u16_at(word, lw)?);
    let pairs = lw + 2 + clw * 4;
    if u16_at(word, pairs)? < 34 {
        return Err("DOC_PIECES_MISSING");
    }
    let clx = pairs + 2 + 33 * 8;
    Ok((
        flags & 0x200 != 0,
        u32_at(word, clx)? as usize,
        u32_at(word, clx + 4)? as usize,
    ))
}

fn decode(word: &[u8], table: &[u8]) -> Result<String, &'static str> {
    let (_, start, size) = table_info(word)?;
    let clx = table
        .get(start..start.checked_add(size).ok_or("DOC_LIMIT")?)
        .ok_or("DOC_TRUNCATED")?;
    let mut at = 0;
    while clx.get(at) == Some(&1) {
        at += 3 + usize::from(u16_at(clx, at + 1)?);
    }
    if clx.get(at) != Some(&2) {
        return Err("DOC_PIECES_MISSING");
    }
    let length = u32_at(clx, at + 1)? as usize;
    if length < 4 || (length - 4) % 12 != 0 {
        return Err("DOC_PIECES_INVALID");
    }
    let pieces = clx.get(at + 5..at + 5 + length).ok_or("DOC_TRUNCATED")?;
    let n = (length - 4) / 12;
    if n > 1_000_000 || u32_at(pieces, 0)? != 0 {
        return Err("DOC_PIECES_INVALID");
    }
    let mut units = Vec::<u16>::new();
    for i in 0..n {
        let first = u32_at(pieces, i * 4)? as usize;
        let last = u32_at(pieces, (i + 1) * 4)? as usize;
        let count = last.checked_sub(first).ok_or("DOC_PIECES_INVALID")?;
        if last > 10_000_000 {
            return Err("DOC_LIMIT");
        }
        let encoded = u32_at(pieces, (n + 1) * 4 + i * 8 + 2)?;
        let compressed = encoded & 0x4000_0000 != 0;
        let offset = (encoded & 0x3fff_ffff) as usize / if compressed { 2 } else { 1 };
        let size = count
            .checked_mul(if compressed { 1 } else { 2 })
            .ok_or("DOC_LIMIT")?;
        let data = word
            .get(offset..offset.checked_add(size).ok_or("DOC_LIMIT")?)
            .ok_or("DOC_TRUNCATED")?;
        if compressed {
            // FcCompressed is Unicode low bytes with the MS-DOC punctuation map;
            // it is not the operating system's ANSI or Cyrillic code page.
            for b in data {
                let ch = match b {
                    0x82 => 0x201a,
                    0x83 => 0x0192,
                    0x84 => 0x201e,
                    0x85 => 0x2026,
                    0x86 => 0x2020,
                    0x87 => 0x2021,
                    0x88 => 0x02c6,
                    0x89 => 0x2030,
                    0x8a => 0x0160,
                    0x8b => 0x2039,
                    0x8c => 0x0152,
                    0x91 => 0x2018,
                    0x92 => 0x2019,
                    0x93 => 0x201c,
                    0x94 => 0x201d,
                    0x95 => 0x2022,
                    0x96 => 0x2013,
                    0x97 => 0x2014,
                    0x98 => 0x02dc,
                    0x99 => 0x2122,
                    0x9a => 0x0161,
                    0x9b => 0x203a,
                    0x9c => 0x0153,
                    0x9f => 0x0178,
                    _ => u16::from(*b),
                };
                units.push(ch);
            }
        } else {
            units.extend(
                data.chunks_exact(2)
                    .map(|v| u16::from_le_bytes([v[0], v[1]])),
            );
        }
    }
    // Keep field results, never field instructions (paths, formulas, commands).
    let mut fields = Vec::new();
    let mut visible = Vec::new();
    for ch in units {
        match ch {
            0x13 => fields.push(false),
            0x14 => {
                if let Some(result) = fields.last_mut() {
                    *result = true;
                }
            }
            0x15 => {
                fields.pop();
            }
            _ if fields.iter().all(|value| *value) => match ch {
                7 | 11 | 12 | 13 => visible.push(10),
                9 | 10 | 32..=0xffff => visible.push(ch),
                _ => (),
            },
            _ => (),
        }
    }
    let text = String::from_utf16(&visible).map_err(|_| "DOC_UTF16_INVALID")?;
    Ok(text.trim().to_owned())
}

pub fn read(path: &Path) -> Result<String, &'static str> {
    if !path.is_absolute() || !path.is_file() {
        return Err("DOCUMENT_PATH_INVALID");
    }
    if path.metadata().map_err(|_| "DOCUMENT_READ_FAILED")?.len() > LIMIT as u64 {
        return Err("DOC_LIMIT");
    }
    #[cfg(windows)]
    {
        let storage = compound::Storage::open(path)?;
        let word = storage.stream("WordDocument")?;
        let (one, _, _) = table_info(&word)?;
        let table = storage.stream(if one { "1Table" } else { "0Table" })?;
        decode(&word, &table)
    }
    #[cfg(not(windows))]
    {
        Err("DOC_PLATFORM_UNAVAILABLE")
    }
}

#[cfg(windows)]
mod compound {
    use super::*;
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn StgOpenStorage(
            path: *const u16,
            priority: *mut c_void,
            mode: u32,
            exclude: *mut c_void,
            reserved: u32,
            storage: *mut *mut c_void,
        ) -> i32;
        fn CoInitializeEx(reserved: *mut c_void, mode: u32) -> i32;
        fn CoUninitialize();
    }
    struct Object(*mut c_void);
    impl Object {
        unsafe fn entry(&self, index: usize) -> *const c_void {
            *(*(self.0 as *const *const *const c_void)).add(index)
        }
    }
    impl Drop for Object {
        fn drop(&mut self) {
            if self.0.is_null() {
                return;
            }
            unsafe {
                let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(self.entry(2));
                release(self.0);
            }
        }
    }
    pub struct Storage {
        object: Object,
        initialized: bool,
    }
    impl Storage {
        pub fn open(path: &Path) -> Result<Self, &'static str> {
            unsafe {
                let initialized = CoInitializeEx(ptr::null_mut(), 0) >= 0;
                let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let mut raw = ptr::null_mut();
                let result = StgOpenStorage(
                    name.as_ptr(),
                    ptr::null_mut(),
                    0x20,
                    ptr::null_mut(),
                    0,
                    &mut raw,
                );
                if result < 0 || raw.is_null() {
                    if initialized {
                        CoUninitialize();
                    }
                    return Err("DOC_CONTAINER_UNREADABLE");
                }
                Ok(Self {
                    object: Object(raw),
                    initialized,
                })
            }
        }
        pub fn stream(&self, name: &str) -> Result<Vec<u8>, &'static str> {
            unsafe {
                let open: unsafe extern "system" fn(
                    *mut c_void,
                    *const u16,
                    *mut c_void,
                    u32,
                    u32,
                    *mut *mut c_void,
                ) -> i32 = std::mem::transmute(self.object.entry(4));
                let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                let mut raw = ptr::null_mut();
                if open(
                    self.object.0,
                    wide.as_ptr(),
                    ptr::null_mut(),
                    0x10,
                    0,
                    &mut raw,
                ) < 0
                    || raw.is_null()
                {
                    return Err("DOC_STREAM_MISSING");
                }
                let stream = Object(raw);
                let read: unsafe extern "system" fn(
                    *mut c_void,
                    *mut c_void,
                    u32,
                    *mut u32,
                ) -> i32 = std::mem::transmute(stream.entry(3));
                let mut result = Vec::new();
                let mut block = vec![0u8; 65536];
                loop {
                    let mut got = 0;
                    if read(
                        stream.0,
                        block.as_mut_ptr().cast(),
                        block.len() as u32,
                        &mut got,
                    ) < 0
                    {
                        return Err("DOC_STREAM_UNREADABLE");
                    }
                    if got as usize > block.len() || result.len() + got as usize > LIMIT {
                        return Err("DOC_LIMIT");
                    }
                    result.extend_from_slice(&block[..got as usize]);
                    if got == 0 {
                        break;
                    }
                }
                Ok(result)
            }
        }
    }
    impl Drop for Storage {
        fn drop(&mut self) {
            // Release storage before ending our COM initialization.
            let object = std::mem::replace(&mut self.object, Object(ptr::null_mut()));
            if !object.0.is_null() {
                drop(object);
            }
            if self.initialized {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
}
