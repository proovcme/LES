//! Unwrap native picture data only; never use the presentation preview as the object.
use super::Result;
const BAD: &str = "DOCX_EMBEDDED_OBJECT_UNSUPPORTED";
fn u32at(bytes: &[u8], at: usize) -> Result<usize> {
    Ok(u32::from_le_bytes(bytes.get(at..at + 4).ok_or(BAD)?.try_into().unwrap()) as usize)
}
pub(super) fn bitmap(native: &[u8]) -> Result<&[u8]> {
    if u32at(native, 0)?.checked_add(4) != Some(native.len()) {
        return Err(BAD);
    }
    let bitmap = &native[4..];
    // Keep all bytes, including the native bitmap's trailing application data.
    if !bitmap.starts_with(b"BM")
        || u32at(bitmap, 2)? < 54
        || u32at(bitmap, 2)? > bitmap.len()
        || u32at(bitmap, 10)? > u32at(bitmap, 2)?
    {
        return Err(BAD);
    }
    Ok(bitmap)
}
pub(super) fn enhanced(contents: &[u8]) -> Result<&[u8]> {
    let header = u32at(contents, 0)?;
    if !(88..=512).contains(&header) {
        return Err(BAD);
    }
    let emf = contents.get(4 + header..).ok_or(BAD)?;
    if u32at(emf, 0)? != 1
        || u32at(emf, 4)? != header
        || u32at(emf, 40)? != 0x464d4520
        || u32at(emf, 48)? != emf.len()
        || contents.get(4..4 + header) != emf.get(..header)
    {
        return Err(BAD);
    }
    // The Windows renderer independently validates every EMF record and escape.
    Ok(emf)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_bounded_native_wrappers_are_unpacked() {
        let mut bmp = vec![0; 58];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&58u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        let mut native = 58u32.to_le_bytes().to_vec();
        native.extend_from_slice(&bmp);
        assert_eq!(bitmap(&native).unwrap(), bmp);
        native[0] = 59;
        assert!(bitmap(&native).is_err());
        let mut emf = vec![0; 108];
        emf[..4].copy_from_slice(&1u32.to_le_bytes());
        emf[4..8].copy_from_slice(&88u32.to_le_bytes());
        emf[40..44].copy_from_slice(&0x464d4520u32.to_le_bytes());
        emf[48..52].copy_from_slice(&108u32.to_le_bytes());
        let mut contents = 88u32.to_le_bytes().to_vec();
        contents.extend_from_slice(&emf[..88]);
        contents.extend_from_slice(&emf);
        assert_eq!(enhanced(&contents).unwrap(), emf);
        contents[8] = 1;
        assert!(enhanced(&contents).is_err());
        assert!(enhanced(&contents[..30]).is_err());
    }
}
