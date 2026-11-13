//! Content-addressed DOCX parts and native OS readers. Never activate an OLE server.
mod raster;
mod vector;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};
type Result<T> = std::result::Result<T, &'static str>;
const BAD: &str = "DOCX_PART_INVALID";
static ID: AtomicU64 = AtomicU64::new(0);
#[derive(Clone)]
struct CachedImage {
    value: Value,
    preview: Vec<u8>,
    bytes: usize,
}
static OCR: OnceLock<Mutex<HashMap<String, CachedImage>>> = OnceLock::new();

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn directory(base: &Path, name: &str) -> Result<PathBuf> {
    let expected = base.join(name);
    if let Ok(meta) = fs::symlink_metadata(&expected) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("DOCX_CACHE_INVALID");
        }
    } else {
        fs::create_dir(&expected).map_err(|_| "DOCX_CACHE_WRITE_FAILED")?;
    }
    if expected.canonicalize().map_err(|_| "DOCX_CACHE_INVALID")? != expected {
        return Err("DOCX_CACHE_INVALID");
    }
    Ok(expected)
}
pub(crate) fn state_root(input: &Value) -> Result<PathBuf> {
    let state = input["owned_state"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LES_STATE_ROOT").map(PathBuf::from))
        .ok_or("DOCX_CACHE_UNAVAILABLE")?;
    if !state.is_absolute() {
        return Err("DOCX_CACHE_INVALID");
    }
    state.canonicalize().map_err(|_| "DOCX_CACHE_UNAVAILABLE")
}
pub(crate) fn storage(state: &Path, source: &str) -> Result<PathBuf> {
    if !hash(source) {
        return Err(BAD);
    }
    let root = directory(state, "storage")?;
    let root = directory(&root, "docx-parts")?;
    directory(&root, source)
}
fn persist(root: &Path, sha: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = root.join(sha);
    if path.exists() {
        let meta = fs::symlink_metadata(&path).map_err(|_| BAD)?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || fs::read(&path).map_err(|_| BAD)? != bytes
        {
            return Err("DOCX_CACHE_INVALID");
        }
        return Ok(path);
    }
    let tmp = Temporary(root.join(format!(
        "part-{}-{}.tmp",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed)
    )));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp.0)
        .map_err(|_| "DOCX_CACHE_WRITE_FAILED")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "DOCX_CACHE_WRITE_FAILED")?;
    drop(file);
    if fs::rename(&tmp.0, &path).is_err() {
        if fs::read(&path).map_err(|_| "DOCX_CACHE_WRITE_FAILED")? != bytes {
            return Err("DOCX_CACHE_INVALID");
        }
    }
    Ok(path)
}
pub(crate) fn read(input: &Value, bytes: &[u8]) -> Result<Value> {
    let kind = input["part_kind"].as_str().ok_or(BAD)?;
    let sha = input["part_sha256"]
        .as_str()
        .filter(|s| hash(s))
        .ok_or(BAD)?;
    let source = input["source_sha256"]
        .as_str()
        .filter(|s| hash(s))
        .ok_or(BAD)?;
    if bytes.len() > 64 * 1024 * 1024 || les_core::sha256_hex(bytes) != sha {
        return Err(BAD);
    }
    let state = state_root(input)?;
    let root = storage(&state, source)?;
    let path = persist(&root, sha, bytes)?;
    eprintln!(
        "{}",
        json!({"event":"document_part_read","kind":kind,"bytes":bytes.len(),"sha256":sha})
    );
    match kind {
        "source" | "binary" | "structure" => Ok(json!({"stored":true,"sha256":sha})),
        "manifest" => {
            let manifest: Value = serde_json::from_slice(bytes).map_err(|_| BAD)?;
            if manifest["schema"] != "les.docx-parts.v1" || manifest["source_sha256"] != source {
                return Err(BAD);
            }
            let pointer = serde_json::to_vec(&json!({"sha256":sha})).map_err(|_| BAD)?;
            let temp = Temporary(root.join(format!(
                "manifest-{}-{}.tmp",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            )));
            fs::write(&temp.0, &pointer).map_err(|_| "DOCX_CACHE_WRITE_FAILED")?;
            fs::rename(&temp.0, root.join("manifest.json"))
                .map_err(|_| "DOCX_CACHE_WRITE_FAILED")?;
            Ok(json!({"stored":true,"sha256":sha}))
        }
        "pdf" => {
            let mut request = input.clone();
            request["path"] = json!(path);
            request["source_sha256"] = json!(sha);
            crate::document_dispatch::external(&request)
        }
        "image" => image(input, bytes, &state),
        _ => Err(BAD),
    }
}
fn image(input: &Value, bytes: &[u8], state: &Path) -> Result<Value> {
    let options = if input["ocr"].is_object() {
        input["ocr"].clone()
    } else if input["ocr_unavailable"] == true {
        return Err("OCR_UNAVAILABLE");
    } else {
        crate::document_dispatch::configured_ocr(Some(state))?
    };
    let helper = options["helper"].as_str().ok_or("OCR_UNAVAILABLE")?;
    let mut helper_file = std::fs::File::open(helper).map_err(|_| "OCR_UNAVAILABLE")?;
    let helper_hash =
        les_core::pdf_source::fingerprint(&mut helper_file).map_err(|_| "OCR_UNAVAILABLE")?;
    let key=les_core::sha256_hex(json!({"schema":"les.docx-ocr.v1","sha":input["part_sha256"],"options":render_options(&input["part_options"]),"ocr":options,"helper":helper_hash}).to_string().as_bytes());
    if let Some(value) = OCR
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| BAD)?
        .get(&key)
        .cloned()
    {
        // A reused image belongs to this source revision too. A cache hit must
        // not publish a preview URL whose bytes exist only under another DOCX.
        persist(
            &storage(state, input["source_sha256"].as_str().ok_or(BAD)?)?,
            value.value["preview_sha256"].as_str().ok_or(BAD)?,
            &value.preview,
        )?;
        return Ok(value.value);
    }
    let (width, height, pixels) = raster::render(bytes, &input["part_options"])?;
    let preview = les_core::document_image::preview_png(width, height, &pixels)?;
    let preview_sha = les_core::sha256_hex(&preview);
    persist(
        &storage(state, input["source_sha256"].as_str().ok_or(BAD)?)?,
        &preview_sha,
        &preview,
    )?;
    eprintln!(
        "{}",
        json!({"event":"document_picture_preview","sha256":preview_sha,"width":width,"height":height})
    );
    if pixels
        .chunks_exact(4)
        .all(|p| p[..3].iter().all(|b| *b >= 254))
    {
        return Ok(
            json!({"text":"","blank":true,"ocr":false,"coverage":"blank_render","preview_sha256":preview_sha}),
        );
    }
    let text = picture_text(&options, width, height, &pixels)?;
    let value = json!({"text":text,"blank":false,"ocr":true,"coverage":if text.is_empty(){"no_recognized_text"}else{"ocr_projection"},"preview_sha256":preview_sha});
    let mut cache = OCR.get_or_init(Default::default).lock().map_err(|_| BAD)?;
    let bytes = preview.len() + value.to_string().len();
    if cache.len() >= 256
        || cache.values().map(|v| v.bytes).sum::<usize>() + bytes > 32 * 1024 * 1024
    {
        cache.clear();
    }
    if bytes <= 32 * 1024 * 1024 {
        cache.insert(
            key,
            CachedImage {
                value: value.clone(),
                preview,
                bytes,
            },
        );
    }
    Ok(value)
}

fn render_options(input: &Value) -> Value {
    let mut value = input.clone();
    if let Some(attributes) = value["attributes"].as_object_mut() {
        attributes.retain(|key, _| {
            matches!(
                key.as_str(),
                "cropleft" | "croptop" | "cropright" | "cropbottom"
            )
        });
    }
    value
}

fn picture_text(options: &Value, width: u32, height: u32, pixels: &[u8]) -> Result<String> {
    let scratch = Path::new(options["scratch"].as_str().ok_or("OCR_UNAVAILABLE")?);
    let ocr = crate::pdf_ocr::Ocr::new(options)?;
    let mut parts = Vec::new();
    let mut length = 0;
    // Keep the original resolution. The Windows reader otherwise downsizes a
    // whole large drawing and loses small labels. Overlap protects tile seams.
    for y in (0..height).step_by(2272) {
        for x in (0..width).step_by(2272) {
            let w = (width - x).min(2400);
            let h = (height - y).min(2400);
            let mut tile = Vec::with_capacity(w as usize * h as usize * 4);
            for row in y..y + h {
                let start = ((row * width + x) * 4) as usize;
                tile.extend_from_slice(&pixels[start..start + w as usize * 4]);
            }
            let tmp = Temporary(scratch.join(format!(
                "page-{}-{}.bmp",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            )));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp.0)
                .map_err(|_| "OCR_READ_FAILED")?;
            crate::pdf_ocr::write_bmp(&mut file, w as i32, h as i32, &tile)
                .map_err(|_| "OCR_READ_FAILED")?;
            drop(file);
            let text = ocr.image(&tmp.0)?;
            length += text.len();
            if length > 2 * 1024 * 1024 {
                return Err("OCR_TEXT_LIMIT");
            }
            if !text.is_empty() {
                parts.push(if width>2400||height>2400{format!("[Область рисунка x={x}, y={y}, ширина={w}, высота={h}; соседние области перекрываются]\n{text}")}else{text});
            }
        }
    }
    Ok(parts.join("\n\n"))
}
