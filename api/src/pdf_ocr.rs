//! Render one page at a time and supervise the configured OCR executable.
use pdfium_render::prelude::*;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const FAILED: &str = "OCR_READ_FAILED";
const RENDER: &str = "OCR_RENDER_FAILED";
const MAX_REPLY: u64 = 1024 * 1024;
static PAGE_ID: AtomicU64 = AtomicU64::new(0);

pub struct Ocr<'a> {
    helper: &'a Path,
    scratch: &'a Path,
    dpi: f64,
    engine: Engine<'a>,
    timeout: Duration,
}

enum Engine<'a> {
    Windows,
    Vision,
    Tesseract { language: &'a str, mode: u64 },
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl<'a> Ocr<'a> {
    pub fn new(options: &'a Value) -> Result<Self, &'static str> {
        let helper = Path::new(options["helper"].as_str().ok_or(FAILED)?);
        let scratch = Path::new(options["scratch"].as_str().ok_or(FAILED)?);
        let dpi = options["dpi"]
            .as_u64()
            .filter(|dpi| (1..=2400).contains(dpi))
            .ok_or(FAILED)? as f64;
        if !helper.is_absolute() || !helper.is_file() || !scratch.is_absolute() || !scratch.is_dir()
        {
            return Err(FAILED);
        }
        let engine = match options["engine"].as_str().unwrap_or("windows") {
            "windows" if cfg!(windows) => Engine::Windows,
            "vision" if cfg!(target_os = "macos") => Engine::Vision,
            "tesseract" => {
                let language = options["language"]
                    .as_str()
                    .filter(|lang| {
                        !lang.is_empty()
                            && lang.len() <= 128
                            && lang
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
                    })
                    .ok_or(FAILED)?;
                let mode = options["psm"].as_u64().filter(|n| *n <= 13).ok_or(FAILED)?;
                Engine::Tesseract { language, mode }
            }
            _ => return Err("OCR_UNAVAILABLE"),
        };
        let timeout = options["timeout_ms"]
            .as_u64()
            .unwrap_or(120_000)
            .clamp(100, 600_000);
        Ok(Self {
            helper,
            scratch,
            dpi,
            engine,
            timeout: Duration::from_millis(timeout),
        })
    }

    pub fn reads_all_pages(&self) -> bool {
        matches!(self.engine, Engine::Tesseract { .. })
    }

    pub fn image(&self, path: &Path) -> Result<String, &'static str> {
        if !path.is_absolute() || !path.is_file() {
            return Err(FAILED);
        }
        let mut command = match self.engine {
            Engine::Windows => {
                let system_root = std::env::var_os("SystemRoot").ok_or(FAILED)?;
                let mut command = Command::new(
                    PathBuf::from(system_root)
                        .join("System32/WindowsPowerShell/v1.0/powershell.exe"),
                );
                command
                    .args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                    ])
                    .arg(self.helper)
                    .arg("-ImagePath")
                    .arg(path);
                command
            }
            Engine::Vision => {
                let mut command = Command::new(self.helper);
                command.arg(path);
                command
            }
            Engine::Tesseract { language, mode } => {
                let mut command = Command::new(self.helper);
                command
                    .arg(path)
                    .args(["stdout", "-l", language, "--psm"])
                    .arg(mode.to_string());
                command
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| FAILED)?;
        let stream = child.stdout.take().expect("piped OCR stdout");
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.take(MAX_REPLY + 1).read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(if result.is_err() {
                        FAILED
                    } else {
                        "OCR_TIMEOUT"
                    });
                }
            }
        };
        let bytes = reader.join().map_err(|_| FAILED)?.map_err(|_| FAILED)?;
        if !status?.success() || bytes.len() as u64 > MAX_REPLY {
            return Err(FAILED);
        }
        if matches!(self.engine, Engine::Tesseract { .. }) {
            return std::str::from_utf8(&bytes)
                .map(|text| text.trim().to_owned())
                .map_err(|_| FAILED);
        }
        let json: Value = serde_json::from_slice(&bytes).map_err(|_| FAILED)?;
        Ok(json["text"].as_str().ok_or(FAILED)?.trim().to_owned())
    }

    pub fn page(
        &self,
        pdf: &dyn PdfiumLibraryBindings,
        page: FPDF_PAGE,
    ) -> Result<Option<String>, &'static str> {
        // The PDF worker owns page and bitmap for the duration of this call.
        let (width, height) = unsafe {
            (
                pdf.FPDF_GetPageWidthF(page) as f64,
                pdf.FPDF_GetPageHeightF(page) as f64,
            )
        };
        if !width.is_finite() || !height.is_finite() || width <= 0. || height <= 0. {
            return Err(RENDER);
        }
        let limit = match self.engine {
            Engine::Windows => 2400.,
            Engine::Vision => 3200.,
            Engine::Tesseract { .. } => 4800.,
        };
        let scale = (self.dpi / 72.).min(limit / width.max(height));
        let width = (width * scale).ceil().clamp(1., limit) as i32;
        let height = (height * scale).ceil().clamp(1., limit) as i32;
        let bitmap = unsafe { pdf.FPDFBitmap_Create(width, height, 0) };
        if bitmap.is_null() {
            return Err(RENDER);
        }
        let result = (|| {
            unsafe {
                pdf.FPDFBitmap_FillRect(bitmap, 0, 0, width, height, 0xffffffff);
                pdf.FPDF_RenderPageBitmap(bitmap, page, 0, 0, width, height, 0, 0);
            }
            let stride = unsafe { pdf.FPDFBitmap_GetStride(bitmap) };
            let buffer = unsafe { pdf.FPDFBitmap_GetBuffer(bitmap) }.cast::<u8>();
            if buffer.is_null() || stride != width * 4 {
                return Err(RENDER);
            }
            let pixels = unsafe { std::slice::from_raw_parts(buffer, (stride * height) as usize) };
            if !self.reads_all_pages() && pixels.chunks_exact(4).all(|pixel| pixel[..3] == [255; 3])
            {
                return Ok(None);
            }
            let nonce = PAGE_ID.fetch_add(1, Ordering::Relaxed);
            let path = self
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
            let text = self.image(&scratch.0)?;
            if text.is_empty() {
                Err("OCR_NO_TEXT")
            } else {
                Ok(Some(text))
            }
        })();
        unsafe { pdf.FPDFBitmap_Destroy(bitmap) };
        result
    }
}

fn write_bmp(
    mut output: impl Write,
    width: i32,
    height: i32,
    pixels: &[u8],
) -> std::io::Result<()> {
    let len = pixels.len() as u32;
    // BI_RGB, 32-bit BGRx, top-down scanlines as returned by PDFium.
    output.write_all(b"BM")?;
    output.write_all(&(54 + len).to_le_bytes())?;
    output.write_all(&[0; 4])?;
    output.write_all(&54_u32.to_le_bytes())?;
    output.write_all(&40_u32.to_le_bytes())?;
    output.write_all(&width.to_le_bytes())?;
    output.write_all(&(-height).to_le_bytes())?;
    output.write_all(&[1, 0, 32, 0])?;
    output.write_all(&[0; 4])?;
    output.write_all(&len.to_le_bytes())?;
    output.write_all(&[0; 16])?;
    output.write_all(pixels)
}
