//! Validate metafile records before handing them to Windows' memory-only renderer.
use super::Result;
use les_core::document_image::Pixels;
const BAD: &str = "DOCX_METAFILE_INVALID";
fn u16at(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        b.get(at..at + 2).ok_or(BAD)?.try_into().unwrap(),
    ))
}
fn u32at(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4).ok_or(BAD)?.try_into().unwrap(),
    ))
}
fn validate(bytes: &[u8]) -> Result<(&[u8], Option<(i32, i32)>)> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("DOCX_IMAGE_LIMIT");
    }
    if bytes.starts_with(&[1, 0, 0, 0]) {
        if bytes.len() < 88
            || u32at(bytes, 40)? != 0x464d4520
            || u32at(bytes, 48)? as usize != bytes.len()
        {
            return Err(BAD);
        }
        let mut at = 0;
        let mut count = 0;
        let mut eof = false;
        while at < bytes.len() {
            let kind = u32at(bytes, at)?;
            let size = u32at(bytes, at + 4)? as usize;
            if size < 8
                || size % 4 != 0
                || at.checked_add(size).is_none_or(|e| e > bytes.len())
                || eof
            {
                return Err(BAD);
            }
            // No printer escape, OpenGL, or named/external colour-profile operations.
            if matches!(kind, 99 | 102 | 103 | 105 | 106 | 110 | 112 | 113 | 122) {
                return Err("DOCX_METAFILE_UNSAFE_OPERATION");
            }
            eof = kind == 14;
            at += size;
            count += 1;
            if count > 1000000 {
                return Err(BAD);
            }
        }
        if !eof || count != u32at(bytes, 52)? {
            return Err(BAD);
        }
        return Ok((bytes, None));
    }
    let (raw, extent) = if bytes.starts_with(&[0xd7, 0xcd, 0xc6, 0x9a]) {
        let mut checksum = 0;
        for at in (0..20).step_by(2) {
            checksum ^= u16at(bytes, at)?;
        }
        if checksum != u16at(bytes, 20)? {
            return Err(BAD);
        }
        let units = u16at(bytes, 14)? as i32;
        if units <= 0 {
            return Err(BAD);
        }
        let x = u16at(bytes, 10)? as i16 as i32 - u16at(bytes, 6)? as i16 as i32;
        let y = u16at(bytes, 12)? as i16 as i32 - u16at(bytes, 8)? as i16 as i32;
        if x <= 0 || y <= 0 {
            return Err(BAD);
        }
        (&bytes[22..], Some((x * 2540 / units, y * 2540 / units)))
    } else {
        (bytes, None)
    };
    if u16at(raw, 2)? != 9 || u32at(raw, 6)? as usize * 2 != raw.len() {
        return Err(BAD);
    }
    let mut at = 18;
    let mut count = 0;
    let mut eof = false;
    let mut window_extent = None;
    while at < raw.len() {
        let size = (u32at(raw, at)? as usize).checked_mul(2).ok_or(BAD)?;
        let kind = u16at(raw, at + 4)?;
        if size < 6 || at.checked_add(size).is_none_or(|e| e > raw.len()) || eof {
            return Err(BAD);
        }
        if kind == 0x626 {
            // MFCOMMENT is data; every executable escape (including SETABORTPROC) is refused.
            if size < 10 || u16at(raw, at + 6)? != 15 {
                return Err("DOCX_METAFILE_UNSAFE_OPERATION");
            }
        }
        if kind == 0x020c && size >= 10 {
            let y = (u16at(raw, at + 6)? as i16 as i32).abs();
            let x = (u16at(raw, at + 8)? as i16 as i32).abs();
            if x > 0 && y > 0 {
                window_extent = Some((x * 2540 / 96, y * 2540 / 96));
            }
        }
        eof = kind == 0;
        at += size;
        count += 1;
        if count > 1000000 {
            return Err(BAD);
        }
    }
    if !eof {
        return Err(BAD);
    }
    Ok((raw, Some(extent.or(window_extent).ok_or(BAD)?)))
}
pub(super) fn render(bytes: &[u8]) -> Result<Pixels> {
    let (bytes, extent) = validate(bytes)?;
    #[cfg(windows)]
    {
        windows::render(bytes, extent)
    }
    #[cfg(not(windows))]
    {
        let _ = (bytes, extent);
        Err("DOCX_METAFILE_UNAVAILABLE")
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{ffi::c_void, ptr};
    type Handle = *mut c_void;
    #[repr(C)]
    struct Startup {
        version: u32,
        callback: *mut c_void,
        background: i32,
        external: i32,
    }
    #[repr(C)]
    struct MetaPicture {
        mode: i32,
        width: i32,
        height: i32,
        handle: Handle,
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn SetEnhMetaFileBits(size: u32, bits: *const u8) -> Handle;
        fn SetWinMetaFileBits(
            size: u32,
            bits: *const u8,
            dc: Handle,
            picture: *const MetaPicture,
        ) -> Handle;
        fn DeleteEnhMetaFile(handle: Handle) -> i32;
    }
    #[link(name = "gdiplus")]
    extern "system" {
        fn GdiplusStartup(token: *mut usize, input: *const Startup, output: Handle) -> i32;
        fn GdiplusShutdown(token: usize);
        fn GdipCreateMetafileFromEmf(emf: Handle, delete: i32, image: *mut Handle) -> i32;
        fn GdipGetImageWidth(image: Handle, width: *mut u32) -> i32;
        fn GdipGetImageHeight(image: Handle, height: *mut u32) -> i32;
        fn GdipDisposeImage(image: Handle) -> i32;
        fn GdipCreateBitmapFromScan0(
            width: i32,
            height: i32,
            stride: i32,
            format: i32,
            scan: *mut u8,
            image: *mut Handle,
        ) -> i32;
        fn GdipGetImageGraphicsContext(image: Handle, graphics: *mut Handle) -> i32;
        fn GdipDeleteGraphics(graphics: Handle) -> i32;
        fn GdipGraphicsClear(graphics: Handle, color: u32) -> i32;
        fn GdipDrawImageRectI(
            graphics: Handle,
            image: Handle,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
        ) -> i32;
    }
    struct Plus(usize);
    impl Drop for Plus {
        fn drop(&mut self) {
            unsafe { GdiplusShutdown(self.0) }
        }
    }
    struct Emf(Handle);
    impl Drop for Emf {
        fn drop(&mut self) {
            unsafe {
                DeleteEnhMetaFile(self.0);
            }
        }
    }
    struct Image(Handle);
    impl Drop for Image {
        fn drop(&mut self) {
            unsafe {
                GdipDisposeImage(self.0);
            }
        }
    }
    struct Graphics(Handle);
    impl Drop for Graphics {
        fn drop(&mut self) {
            unsafe {
                GdipDeleteGraphics(self.0);
            }
        }
    }
    fn okay(status: i32) -> Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err("DOCX_METAFILE_RENDER_FAILED")
        }
    }
    pub(super) fn render(bytes: &[u8], extent: Option<(i32, i32)>) -> Result<Pixels> {
        let mut token = 0;
        okay(unsafe {
            GdiplusStartup(
                &mut token,
                &Startup {
                    version: 1,
                    callback: ptr::null_mut(),
                    background: 0,
                    external: 1,
                },
                ptr::null_mut(),
            )
        })?;
        let _plus = Plus(token);
        let emf = match extent {
            None => unsafe { SetEnhMetaFileBits(bytes.len() as u32, bytes.as_ptr()) },
            Some((w, h)) => unsafe {
                SetWinMetaFileBits(
                    bytes.len() as u32,
                    bytes.as_ptr(),
                    ptr::null_mut(),
                    &MetaPicture {
                        mode: 8,
                        width: w,
                        height: h,
                        handle: ptr::null_mut(),
                    },
                )
            },
        };
        if emf.is_null() {
            return Err(BAD);
        }
        let emf = Emf(emf);
        let mut image = ptr::null_mut();
        okay(unsafe { GdipCreateMetafileFromEmf(emf.0, 0, &mut image) })?;
        let image = Image(image);
        let (mut width, mut height) = (0, 0);
        okay(unsafe { GdipGetImageWidth(image.0, &mut width) })?;
        okay(unsafe { GdipGetImageHeight(image.0, &mut height) })?;
        if width == 0 || height == 0 {
            return Err(BAD);
        }
        let scale = (2400. / width.max(height) as f64).min(4.0);
        let width = (width as f64 * scale).round().max(1.) as u32;
        let height = (height as f64 * scale).round().max(1.) as u32;
        if width as u64 * height as u64 > 12_000_000 {
            return Err("DOCX_IMAGE_LIMIT");
        }
        let mut pixels = vec![255; width as usize * height as usize * 4];
        let mut bitmap = ptr::null_mut();
        okay(unsafe {
            GdipCreateBitmapFromScan0(
                width as i32,
                height as i32,
                width as i32 * 4,
                0x26200a,
                pixels.as_mut_ptr(),
                &mut bitmap,
            )
        })?;
        let bitmap = Image(bitmap);
        let mut graphics = ptr::null_mut();
        okay(unsafe { GdipGetImageGraphicsContext(bitmap.0, &mut graphics) })?;
        let graphics = Graphics(graphics);
        okay(unsafe { GdipGraphicsClear(graphics.0, 0xffffffff) })?;
        okay(unsafe {
            GdipDrawImageRectI(graphics.0, image.0, 0, 0, width as i32, height as i32)
        })?;
        drop(graphics);
        drop(bitmap);
        Ok(Pixels {
            width,
            height,
            bgra: pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_and_truncated_metafiles_never_reach_gdi() {
        assert!(validate(&[]).is_err());
        let mut emf = vec![0; 112];
        emf[..4].copy_from_slice(&1u32.to_le_bytes());
        emf[4..8].copy_from_slice(&88u32.to_le_bytes());
        emf[40..44].copy_from_slice(&0x464d4520u32.to_le_bytes());
        emf[48..52].copy_from_slice(&112u32.to_le_bytes());
        emf[52..56].copy_from_slice(&3u32.to_le_bytes());
        emf[88..92].copy_from_slice(&105u32.to_le_bytes());
        emf[92..96].copy_from_slice(&12u32.to_le_bytes());
        emf[100..104].copy_from_slice(&14u32.to_le_bytes());
        emf[104..108].copy_from_slice(&12u32.to_le_bytes());
        assert_eq!(validate(&emf).err(), Some("DOCX_METAFILE_UNSAFE_OPERATION"));
    }
}
