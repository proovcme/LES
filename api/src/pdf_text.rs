//! Bounded PDFium reader executed in a disposable child, with no OCR or table inference.
use pdfium_render::prelude::*;
use serde_json::{json, Value};
use std::{
    ffi::{c_int, c_ulong, c_void},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_PDF_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 12 * 1024 * 1024;

struct Library(Box<dyn PdfiumLibraryBindings>);

impl Library {
    fn open(asset: &Path) -> Result<Self, &'static str> {
        if !asset.is_absolute() {
            return Err("PDF_PATH_INVALID");
        }
        let bindings = Pdfium::bind_to_library(asset).map_err(|_| "PDFIUM_UNAVAILABLE")?;
        // A single-threaded worker owns the library for its whole lifetime.
        unsafe { bindings.FPDF_InitLibrary() };
        Ok(Self(bindings))
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // The document and all pages have been closed before dropping the library.
        unsafe { self.0.FPDF_DestroyLibrary() }
    }
}

fn page_text(pdf: &dyn PdfiumLibraryBindings, page: FPDF_PAGE) -> Result<String, &'static str> {
    // The page belongs to a live document in this single-threaded worker.
    let text = unsafe { pdf.FPDFText_LoadPage(page) };
    if text.is_null() {
        return Err("PDF_TEXT_UNAVAILABLE");
    }
    let result = text_range(pdf, text);
    unsafe { pdf.FPDFText_ClosePage(text) };
    result
}

fn text_range(
    pdf: &dyn PdfiumLibraryBindings,
    text: FPDF_TEXTPAGE,
) -> Result<String, &'static str> {
    // A text-page handle is valid for the duration of this read.
    let count = unsafe { pdf.FPDFText_CountChars(text) };
    if !(0..=2_000_000).contains(&count) {
        return Err("PDF_TEXT_LIMIT");
    }
    let mut start = 0;
    let mut end = count - 1;
    while start <= end && unsafe { pdf.FPDFText_GetTextIndexFromCharIndex(text, start) } < 0 {
        start += 1;
    }
    while start <= end && unsafe { pdf.FPDFText_GetTextIndexFromCharIndex(text, end) } < 0 {
        end -= 1;
    }
    if start > end {
        return Ok(String::new());
    }
    let first = unsafe { pdf.FPDFText_GetTextIndexFromCharIndex(text, start) };
    let last = unsafe { pdf.FPDFText_GetTextIndexFromCharIndex(text, end) };
    let length = (last - first + 2).max(end - start + 2);
    if !(1..=4_000_000).contains(&length) {
        return Err("PDF_TEXT_LIMIT");
    }
    let mut buffer = vec![0_u16; length as usize];
    // Allocate for both the requested range and PDFium's mapped output, plus NUL.
    let written =
        unsafe { pdf.FPDFText_GetText(text, start, end - start + 1, buffer.as_mut_ptr()) };
    if !(1..=length).contains(&written) {
        return Err("PDF_TEXT_UNAVAILABLE");
    }
    let value: String = char::decode_utf16(buffer[..written as usize - 1].iter().copied())
        .filter_map(Result::ok)
        .collect();
    Ok(value
        .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .to_owned())
}

fn scan_objects(
    pdf: &dyn PdfiumLibraryBindings,
    page: FPDF_PAGE,
    form: Option<FPDF_PAGEOBJECT>,
    level: u8,
    empty: bool,
) -> Result<bool, &'static str> {
    // PDFium owns page objects and nested forms while the page is alive.
    let count = unsafe {
        match form {
            Some(form) => pdf.FPDFFormObj_CountObjects(form),
            None => pdf.FPDFPage_CountObjects(page),
        }
    };
    if count < 0 {
        return Err("PDF_OBJECTS_UNAVAILABLE");
    }
    for index in 0..count {
        let object = unsafe {
            match form {
                Some(form) => pdf.FPDFFormObj_GetObject(form, index as _),
                None => pdf.FPDFPage_GetObject(page, index),
            }
        };
        if object.is_null() {
            return Err("PDF_OBJECTS_UNAVAILABLE");
        }
        let kind = unsafe { pdf.FPDFPageObj_GetType(object) } as u32;
        if kind == FPDF_PAGEOBJ_IMAGE || empty && kind == FPDF_PAGEOBJ_PATH {
            return Ok(true);
        }
        if kind == FPDF_PAGEOBJ_FORM
            && level < 14
            && scan_objects(pdf, page, Some(object), level + 1, empty)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_pages(
    pdf: &dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
    ocr: Option<&crate::pdf_ocr::Ocr<'_>>,
) -> Result<Value, &'static str> {
    let count = unsafe { pdf.FPDF_GetPageCount(document) };
    if !(0..=10_000).contains(&count) {
        return Err("PDF_PAGE_LIMIT");
    }
    let mut parts = Vec::new();
    let mut bytes = 0;
    let mut needs_ocr = false;
    for index in 0..count {
        let page = unsafe { pdf.FPDF_LoadPage(document, index) };
        if page.is_null() {
            return Err("PDF_PAGE_UNAVAILABLE");
        }
        let result = (|| {
            let mut text = page_text(pdf, page)?;
            let scan = text.chars().take(20).count() < 20
                && scan_objects(pdf, page, None, 0, text.is_empty())?;
            needs_ocr |= scan;
            if let Some(ocr) = ocr.filter(|ocr| scan || ocr.reads_all_pages()) {
                text = ocr
                    .page(pdf, page)
                    .inspect_err(|code| {
                        eprintln!(
                            "{}",
                            json!({"event":"pdf_ocr_failed","page":index+1,"reason":code})
                        );
                    })?
                    .unwrap_or_default();
            }
            if !text.is_empty() {
                bytes += text.len() + 32;
                if bytes > MAX_TEXT_BYTES {
                    return Err("PDF_TEXT_LIMIT");
                }
                parts.push(format!("## Стр. {}\n\n{text}", index + 1));
            }
            Ok(())
        })();
        unsafe { pdf.FPDF_ClosePage(page) };
        result?;
    }
    Ok(json!({"text":parts.join("\n\n"),"needs_ocr":needs_ocr,"pages":count,"ocr":ocr.is_some()}))
}

struct Input {
    file: File,
    length: u64,
}

unsafe extern "C" fn read_at(
    context: *mut c_void,
    offset: c_ulong,
    buffer: *mut u8,
    size: c_ulong,
) -> c_int {
    if context.is_null() || buffer.is_null() || size == 0 {
        return 0;
    }
    // PDFium calls back synchronously with our live Input and its own output buffer.
    let input = unsafe { &mut *context.cast::<Input>() };
    let offset = offset as u64;
    let size = size as usize;
    if size > isize::MAX as usize
        || offset
            .checked_add(size as u64)
            .is_none_or(|end| end > input.length)
    {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts_mut(buffer, size) };
    i32::from(
        input
            .file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| input.file.read_exact(data))
            .is_ok(),
    )
}

fn read(library: &Library, path: &Path, options: &Value) -> Result<Value, &'static str> {
    if !path.is_absolute() {
        return Err("PDF_PATH_INVALID");
    }
    let pdf = library.0.as_ref();
    let ocr = (!options["ocr"].is_null())
        .then(|| crate::pdf_ocr::Ocr::new(&options["ocr"]))
        .transpose()?;
    if options["kind"] == "image_ocr" {
        if let Some(ocr) = ocr {
            return Ok(json!({"ocr":true,"text":ocr.image(path)?}));
        }
        return Err("OCR_UNAVAILABLE");
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep the same bytes alive while PDFium reads them. No write/delete sharing.
        open.share_mode(1);
    }
    let mut file = open.open(path).map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
    if let Some(hash) = options["source_sha256"].as_str() {
        les_core::pdf_source::verify(&mut file, hash)?;
    }
    let metadata = file.metadata().map_err(|_| "PDF_SOURCE_UNAVAILABLE")?;
    if !metadata.is_file() || metadata.len() > MAX_PDF_BYTES {
        return Err("PDF_SOURCE_LIMIT");
    }
    let mut input = Input {
        file,
        length: metadata.len(),
    };
    let mut access = FPDF_FILEACCESS {
        m_FileLen: input.length.try_into().map_err(|_| "PDF_SOURCE_LIMIT")?,
        m_GetBlock: Some(read_at),
        m_Param: (&mut input as *mut Input).cast(),
    };
    // Keep both access and input alive and stationary until the document is closed.
    let document = unsafe { pdf.FPDF_LoadCustomDocument(&mut access, None) };
    if document.is_null() {
        return Err("PDF_OPEN_FAILED");
    }
    let result = if options["kind"] == "source_page" {
        read_page(pdf, document, options)
    } else {
        read_pages(pdf, document, ocr.as_ref())
    };
    unsafe { pdf.FPDF_CloseDocument(document) };
    if let Some(hash) = options["source_sha256"].as_str() {
        les_core::pdf_source::verify(&mut input.file, hash)?;
    }
    result
}

fn read_page(
    pdf: &dyn PdfiumLibraryBindings,
    document: FPDF_DOCUMENT,
    options: &Value,
) -> Result<Value, &'static str> {
    let number = options["source"]["page"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("PDF_PAGE_REQUIRED")?;
    let count = unsafe { pdf.FPDF_GetPageCount(document) };
    if !(0..=10_000).contains(&count) {
        return Err("PDF_PAGE_LIMIT");
    }
    if number > count as u64 {
        return Err("PDF_PAGE_OUT_OF_RANGE");
    }
    let offset = options["source"]
        .get("offset")
        .map_or(Some(0), Value::as_u64)
        .ok_or("PDF_OFFSET_INVALID")?;
    let limit = options["source"]
        .get("max_chars")
        .map_or(Some(6000), Value::as_u64)
        .filter(|n| (1..=12000).contains(n))
        .ok_or("PDF_TEXT_LIMIT")?;
    let page = unsafe { pdf.FPDF_LoadPage(document, number as i32 - 1) };
    if page.is_null() {
        return Err("PDF_PAGE_UNAVAILABLE");
    }
    let result = (|| {
        let text = page_text(pdf, page)?;
        if text.chars().take(20).count() < 20 && scan_objects(pdf, page, None, 0, text.is_empty())?
        {
            return Err("PDF_OCR_REQUIRED");
        }
        if text.is_empty() {
            return Err("PDF_PAGE_EMPTY");
        }
        let count_chars = text.chars().count() as u64;
        if offset >= count_chars {
            return Err("PDF_OFFSET_INVALID");
        }
        let query = options["source"]["q"].as_str().unwrap_or("").trim();
        let (offset, end) = if query.is_empty() {
            (offset, (offset + limit).min(count_chars))
        } else {
            let (start, end) = les_core::pdf_source::literal_window(&text, query, limit as usize)?;
            (start as u64, end as u64)
        };
        let excerpt: String = text
            .chars()
            .skip(offset as usize)
            .take((end - offset) as usize)
            .collect();
        let next = offset + excerpt.chars().count() as u64;
        Ok(
            json!({"text":excerpt,"page":number,"page_count":count,"offset":offset,
            "next_offset":if query.is_empty() && next<count_chars {Some(next)} else {None},"complete":next==count_chars && offset==0,
            "selection":if query.is_empty(){"page_range"}else{"literal_match"},"matched_query":query,
            "total_chars":count_chars,"extractor":"les.pdfium.page.v1","source_sha256":options["source_sha256"]}),
        )
    })();
    unsafe { pdf.FPDF_ClosePage(page) };
    result
}

fn source_page(library: &Library, options: &Value) -> Result<Value, &'static str> {
    let source = les_core::pdf_source::resolve(options)?;
    let mut input = options.clone();
    input["source_sha256"] = json!(source.sha256);
    let mut value = read(library, &source.path, &input)?;
    for (key, item) in source.identity.as_object().ok_or("PDF_IDENTITY_REQUIRED")? {
        value[key] = item.clone();
    }
    Ok(value)
}

fn response(result: Result<Value, &'static str>) -> Value {
    match result {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(code) if code.starts_with("OCR_") => json!({"status":"failed","code":code}),
        Err(code) => json!({"status":"compatibility","code":code}),
    }
}

pub fn extract(asset: &Path, path: &Path) -> Value {
    response(Library::open(asset).and_then(|library| read(&library, path, &Value::Null)))
}

pub(crate) fn convert(asset: &Path, path: &Path, options: &Value) -> Result<Value, &'static str> {
    Library::open(asset).and_then(|library| read(&library, path, options))
}

pub fn serve(asset: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let library = Library::open(asset).map_err(std::io::Error::other)?;
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    let mut frame = Vec::new();
    loop {
        frame.clear();
        if (&mut input).take(64 * 1024).read_until(b'\n', &mut frame)? == 0 {
            return Ok(());
        }
        if !frame.ends_with(b"\n") {
            return Err(std::io::Error::other("PDF_INPUT_LIMIT").into());
        }
        let envelope: Value = serde_json::from_slice(&frame)?;
        let path = envelope["input"]["path"]
            .as_str()
            .ok_or("PDF_PATH_INVALID")
            .map(Path::new);
        let result = if envelope["input"]["kind"] == "source_page" {
            match source_page(&library, &envelope["input"]) {
                Ok(value) => json!({"status":"completed","value":value}),
                Err(code) => json!({"status":"failed","code":code}),
            }
        } else {
            response(path.and_then(|path| read(&library, path, &envelope["input"])))
        };
        serde_json::to_writer(
            &mut output,
            &json!({"request_id":envelope["request_id"],"result":result}),
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}
