//! Render one page at a time and supervise the configured OCR executable.
mod render;
mod scratch;
use pdfium_render::prelude::*;
pub(crate) use scratch::cleanup;
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
    persistent_revision: Option<String>,
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
        let persistent_revision = if matches!(engine, Engine::Windows)
            && fs::metadata(helper)
                .map(|m| m.len() <= 65536)
                .unwrap_or(false)
        {
            let bytes = fs::read(helper).map_err(|_| FAILED)?;
            bytes
                .starts_with(b"# les.ocr-stdio.v1")
                .then(|| les_core::sha256_hex(&bytes))
        } else {
            None
        };
        Ok(Self {
            helper,
            scratch,
            dpi,
            engine,
            timeout: Duration::from_millis(timeout),
            persistent_revision,
        })
    }

    pub fn reads_all_pages(&self) -> bool {
        matches!(self.engine, Engine::Tesseract { .. })
    }

    pub fn image(&self, path: &Path) -> Result<String, &'static str> {
        if !path.is_absolute() || !path.is_file() {
            return Err(FAILED);
        }
        if let Some(revision) = &self.persistent_revision {
            return self.persistent_image(path, revision);
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
                    .arg(winrt_path(path));
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

    fn persistent_image(&self, path: &Path, revision: &str) -> Result<String, &'static str> {
        let program = PathBuf::from(std::env::var_os("SystemRoot").ok_or(FAILED)?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let reply = les_core::run_native_ocr(&serde_json::json!({
            "schema":"les.worker-host.v1", "program":program,
            "directory":program.parent().ok_or(FAILED)?,
            "args":["-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass",
                "-File",self.helper,"-Serve"],
            "context":{"protocol":"les.ocr-stdio.v1","helper_sha256":revision},
            "timeout_ms":self.timeout.as_millis() as u64,
            "input":{"image_path":winrt_path(path)}
        }))
        .map_err(|_| FAILED)?;
        if reply["status"] != "completed" || reply["result"]["status"] != "completed" {
            let code = reply["code"].as_str().unwrap_or(FAILED);
            eprintln!(
                "{}",
                serde_json::json!({"event":"ocr_worker_failed","code":code,
                "worker_pid":reply["worker_pid"],"diagnostics":reply["diagnostics"]})
            );
            return Err(
                if matches!(code, "WORKER_TIMEOUT" | "WORKER_QUEUE_TIMEOUT") {
                    "OCR_TIMEOUT"
                } else {
                    FAILED
                },
            );
        }
        let text = reply["result"]["value"]["text"].as_str().ok_or(FAILED)?;
        if text.len() as u64 > MAX_REPLY {
            return Err(FAILED);
        }
        Ok(text.trim().to_owned())
    }

    pub fn page(
        &self,
        pdf: &dyn PdfiumLibraryBindings,
        page: FPDF_PAGE,
    ) -> Result<Option<String>, &'static str> {
        render::read(self, pdf, page)
    }
}

fn winrt_path(path: &Path) -> PathBuf {
    // StorageFile.GetFileFromPathAsync takes a normal absolute Windows path,
    // not the extended-length spelling returned by Rust's canonicalize().
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

pub(crate) fn write_bmp(
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn windows_ocr_reuses_session_but_never_turns_a_bad_image_into_empty_success() {
        let scratch = std::env::temp_dir().join(format!("les-ocr-protocol-{}", std::process::id()));
        fs::create_dir_all(&scratch).unwrap();
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tools/light_windows_ocr.ps1");
        let options =
            serde_json::json!({"helper":helper,"scratch":scratch,"dpi":300,"engine":"windows"});
        let ocr = Ocr::new(&options).unwrap();
        assert!(ocr.persistent_revision.is_some());
        let image = scratch.join("blank.bmp");
        write_bmp(
            File::create(&image).unwrap(),
            100,
            40,
            &vec![255; 100 * 40 * 4],
        )
        .unwrap();
        let invalid = scratch.join("broken.bmp");
        fs::write(&invalid, b"not a bitmap").unwrap();
        assert_eq!(ocr.image(&image).unwrap(), "");
        assert_eq!(ocr.image(&invalid).unwrap_err(), FAILED);
        assert_eq!(ocr.image(&image).unwrap(), "");
        fs::remove_file(image).unwrap();
        fs::remove_file(invalid).unwrap();
        fs::remove_dir(scratch).unwrap();
    }
}
