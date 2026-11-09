//! Render a verified physical page or a clipped rectangle in the PDF worker.
use super::*;
const DRAW_ANNOTATIONS: i32 = 0x01; // FPDF_ANNOT from PDFium's fpdfview.h.

pub(super) fn render(
    pdf: &dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
    options: &Value,
) -> Result<Value, &'static str> {
    let request = &options["source"];
    let number = request["page"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("PDF_PAGE_REQUIRED")?;
    let count = unsafe { pdf.FPDF_GetPageCount(document) };
    if !(0..=10000).contains(&count) || number > count as u64 {
        return Err("PDF_PAGE_OUT_OF_RANGE");
    }
    let page = unsafe { pdf.FPDF_LoadPage(document, number as i32 - 1) };
    if page.is_null() {
        return Err("PDF_PAGE_UNAVAILABLE");
    }
    let result = render_page(pdf, page, request);
    unsafe { pdf.FPDF_ClosePage(page) };
    Ok(json!({"png":result?,"page":number,"source_sha256":options["source_sha256"]}))
}

fn render_page(
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    request: &Value,
) -> Result<String, &'static str> {
    let w = unsafe { pdf.FPDF_GetPageWidthF(page) } as f64;
    let h = unsafe { pdf.FPDF_GetPageHeightF(page) } as f64;
    if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. {
        return Err("PDF_PREVIEW_FAILED");
    }
    let max = request["width"]
        .as_u64()
        .filter(|v| (320..=1800).contains(v))
        .ok_or("PDF_PREVIEW_WIDTH_INVALID")? as f64;
    let (x0, y0, x1, y1) = if request["bbox"].is_null() {
        (0., 0., w, h)
    } else {
        let bbox = request["bbox"]
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or("PDF_PREVIEW_BBOX_INVALID")?;
        let read = |i: usize| {
            bbox[i]
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or("PDF_PREVIEW_BBOX_INVALID")
        };
        let bounds = (
            read(0)?.max(0.),
            read(1)?.max(0.),
            read(2)?.min(w),
            read(3)?.min(h),
        );
        if bounds.2 - bounds.0 < 1. || bounds.3 - bounds.1 < 1. {
            return Err("PDF_PREVIEW_BBOX_INVALID");
        }
        bounds
    };
    // Keep the entire render bounded even for engineering sheets. The PNG and
    // its worker frame have independent byte limits.
    let scale = (max / (x1 - x0)).clamp(0.5, 3.).min(2200. / w.max(h));
    let width = (w * scale).ceil().clamp(1., 2200.) as i32;
    let height = (h * scale).ceil().clamp(1., 2200.) as i32;
    let left = ((x0 * scale).round() as i32).clamp(0, width - 1);
    let top = ((y0 * scale).round() as i32).clamp(0, height - 1);
    let right = ((x1 * scale).ceil() as i32).clamp(left + 1, width);
    let bottom = ((y1 * scale).ceil() as i32).clamp(top + 1, height);
    let bitmap = unsafe { pdf.FPDFBitmap_Create(width, height, 0) };
    if bitmap.is_null() {
        return Err("PDF_PREVIEW_FAILED");
    }
    let result = (|| {
        unsafe {
            pdf.FPDFBitmap_FillRect(bitmap, 0, 0, width, height, 0xffffffff);
            pdf.FPDF_RenderPageBitmap(bitmap, page, 0, 0, width, height, 0, DRAW_ANNOTATIONS);
        }
        let stride = unsafe { pdf.FPDFBitmap_GetStride(bitmap) };
        let buffer = unsafe { pdf.FPDFBitmap_GetBuffer(bitmap) }.cast::<u8>();
        if buffer.is_null() || stride != width * 4 {
            return Err("PDF_PREVIEW_FAILED");
        }
        let pixels = unsafe { std::slice::from_raw_parts(buffer, (stride * height) as usize) };
        let mut rgb = Vec::with_capacity(((right - left) * (bottom - top) * 3) as usize);
        for row in top..bottom {
            let start = (row * stride + left * 4) as usize;
            let end = (row * stride + right * 4) as usize;
            for pixel in pixels[start..end].chunks_exact(4) {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
        }
        les_core::pdf_source::encode_png(&rgb, (right - left) as u32, (bottom - top) as u32)
    })();
    unsafe { pdf.FPDFBitmap_Destroy(bitmap) };
    result
}
