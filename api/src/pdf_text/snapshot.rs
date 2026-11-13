//! One physical page, including its resources. Never renumber an original source.
use super::*;
#[repr(C)]
struct Output {
    interface: FPDF_FILEWRITE,
    bytes: Vec<u8>,
}
unsafe extern "C" fn block(
    writer: *mut FPDF_FILEWRITE,
    data: *const c_void,
    size: c_ulong,
) -> c_int {
    if writer.is_null() || data.is_null() {
        return 0;
    }
    // The callback runs synchronously while this repr(C) owner remains live.
    let out = unsafe { &mut *writer.cast::<Output>() };
    let size = size as usize;
    if size > isize::MAX as usize
        || out
            .bytes
            .len()
            .checked_add(size)
            .is_none_or(|n| n > 128 * 1024 * 1024)
    {
        return 0;
    }
    out.bytes
        .extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) });
    1
}
pub(super) fn page(
    pdf: &dyn PdfiumLibraryBindings,
    original: FPDF_DOCUMENT,
    page: u64,
) -> Result<Vec<u8>, &'static str> {
    let index = i32::try_from(page - 1).map_err(|_| "PDF_PAGE_OUT_OF_RANGE")?;
    let copy = unsafe { pdf.FPDF_CreateNewDocument() };
    if copy.is_null() {
        return Err("PDF_SOURCE_SNAPSHOT_FAILED");
    }
    let mut out = Output {
        interface: FPDF_FILEWRITE {
            version: 1,
            WriteBlock: Some(block),
        },
        bytes: Vec::new(),
    };
    let ok = unsafe { pdf.FPDF_ImportPagesByIndex(copy, original, &index, 1, 0) } != 0
        && unsafe { pdf.FPDF_SaveAsCopy(copy, &mut out.interface, 0) } != 0;
    unsafe { pdf.FPDF_CloseDocument(copy) };
    if !ok {
        return Err("PDF_SOURCE_SNAPSHOT_FAILED");
    }
    Ok(out.bytes)
}
