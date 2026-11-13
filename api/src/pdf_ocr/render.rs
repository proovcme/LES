//! Bounded orientation and resolution retries, retaining the physical source page.
use super::*;

pub(super) fn read(
    ocr: &Ocr<'_>,
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
) -> Result<Option<String>, &'static str> {
    let (width, height) = unsafe {
        (
            pdf.FPDF_GetPageWidthF(page) as f64,
            pdf.FPDF_GetPageHeightF(page) as f64,
        )
    };
    if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
        return Err(RENDER);
    }
    let limit = match ocr.engine {
        Engine::Windows => 2400,
        Engine::Vision => 3200,
        Engine::Tesseract { .. } => 4800,
    };
    let scale = (ocr.dpi / 72.).min(limit as f64 / width.max(height));
    let size = (
        (width * scale).ceil().max(1.) as i32,
        (height * scale).ceil().max(1.) as i32,
    );
    let deadline = Instant::now() + ocr.timeout;
    let first = region(ocr, pdf, page, size, (0, 0), size, 0)?;
    if first.as_ref().is_none_or(|s| useful(s)) {
        return Ok(first);
    }
    let mut best = first.unwrap_or_default();
    let mut best_rotation = 0;
    for rotation in [1, 3, 2] {
        if Instant::now() >= deadline {
            return Err("OCR_TIMEOUT");
        }
        let canvas = if rotation % 2 == 0 {
            size
        } else {
            (size.1, size.0)
        };
        if let Some(text) =
            region(ocr, pdf, page, canvas, (0, 0), canvas, rotation)?.filter(|s| !s.is_empty())
        {
            if score(&text) > score(&best) {
                best = text;
                best_rotation = rotation;
            }
        }
    }
    // Large drawings lose small labels when the whole sheet is reduced. Retry
    // Overlapping regions retain at least twice the first render's resolution
    // and the requested DPI on large sheets, up to a 7200-pixel long edge.
    // Memory stays bounded by one OCR bitmap; the file deadline still applies.
    let retry_scale = (scale * 2.)
        .max(ocr.dpi / 72.)
        .min(7200. / width.max(height));
    let retry_size = (
        (width * retry_scale).ceil().max(1.) as i32,
        (height * retry_scale).ceil().max(1.) as i32,
    );
    let step = limit - 128;
    for rotation in [0, 1, 3, 2] {
        let canvas = if rotation % 2 == 0 {
            retry_size
        } else {
            (retry_size.1, retry_size.0)
        };
        let mut parts = Vec::new();
        let mut bytes = 0;
        for y in (0..canvas.1).step_by(step as usize) {
            for x in (0..canvas.0).step_by(step as usize) {
                if Instant::now() >= deadline {
                    return Err("OCR_TIMEOUT");
                }
                let tile = ((canvas.0 - x).min(limit), (canvas.1 - y).min(limit));
                if let Some(text) = region(ocr, pdf, page, canvas, (x, y), tile, rotation)?
                    .filter(|s| !s.is_empty())
                {
                    bytes += text.len();
                    if bytes > MAX_REPLY as usize {
                        return Err("OCR_TEXT_LIMIT");
                    }
                    // Only exact repeated tile results are redundant. Never merge
                    // similar lines or infer a table across a drawing boundary.
                    if !parts.contains(&text) {
                        parts.push(text);
                    }
                }
            }
        }
        let text = parts.join("\n\n");
        if score(&text) > score(&best) {
            best = text;
            best_rotation = rotation;
        }
    }
    if !useful(&best) {
        Err("OCR_NO_TEXT")
    } else {
        eprintln!(
            "{}",
            serde_json::json!({"event":"pdf_ocr_retry_completed","quarter_turns":best_rotation,"text_chars":best.chars().count()})
        );
        Ok(Some(best))
    }
}

fn score(text: &str) -> usize {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(|word| word.chars().count())
        .sum()
}
fn useful(text: &str) -> bool {
    // A few incidental glyphs after rotating a drawing are not evidence of
    // successful text extraction. This is only an OCR admission guard, never
    // a factual or table-layout validator.
    score(text) >= 8
}

fn region(
    ocr: &Ocr<'_>,
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    canvas: (i32, i32),
    origin: (i32, i32),
    size: (i32, i32),
    rotation: i32,
) -> Result<Option<String>, &'static str> {
    let (width, height) = size;
    let bitmap = unsafe { pdf.FPDFBitmap_Create(width, height, 0) };
    if bitmap.is_null() {
        return Err(RENDER);
    }
    let result = (|| {
        unsafe {
            pdf.FPDFBitmap_FillRect(bitmap, 0, 0, width, height, 0xffffffff);
            pdf.FPDF_RenderPageBitmap(
                bitmap, page, -origin.0, -origin.1, canvas.0, canvas.1, rotation, 0,
            );
        }
        let stride = unsafe { pdf.FPDFBitmap_GetStride(bitmap) };
        let buffer = unsafe { pdf.FPDFBitmap_GetBuffer(bitmap) }.cast::<u8>();
        if buffer.is_null() || stride != width * 4 {
            return Err(RENDER);
        }
        let pixels = unsafe { std::slice::from_raw_parts(buffer, (stride * height) as usize) };
        if !ocr.reads_all_pages() && pixels.chunks_exact(4).all(|pixel| pixel[..3] == [255; 3]) {
            return Ok(None);
        }
        let nonce = PAGE_ID.fetch_add(1, Ordering::Relaxed);
        let path = ocr
            .scratch
            .join(format!("page-{}-{nonce}.bmp", std::process::id()));
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| RENDER)?;
        let scratch = Scratch(path);
        write_bmp(&mut file, width, height, pixels).map_err(|_| RENDER)?;
        drop(file);
        Ok(Some(ocr.image(&scratch.0)?))
    })();
    unsafe { pdf.FPDFBitmap_Destroy(bitmap) };
    result
}
