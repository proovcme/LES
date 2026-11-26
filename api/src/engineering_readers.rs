//! Read-only process adapters for engineering files in the regular format worker.
mod dwg_tables;
mod projection;

use projection::{dwg_objects, rvt_classes, rvt_context, rvt_object, rvt_saved_label};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, &'static str>;
const UNAVAILABLE: &str = "ENGINEERING_READER_UNAVAILABLE";
const ASSET: &str = "ENGINEERING_READER_ASSET_INVALID";
const FAILED: &str = "ENGINEERING_READER_FAILED";
const LIMIT: &str = "ENGINEERING_READER_LIMIT";
const MAX_OUTPUT: u64 = 128 * 1024 * 1024;
const MAX_OBJECTS: usize = 20_000;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
static READERS: OnceLock<Result<Readers>> = OnceLock::new();

struct Readers {
    root: PathBuf,
    revision: String,
    dwg: bool,
    rvt: bool,
    ifc: bool,
}

impl Readers {
    fn load() -> Result<Self> {
        let root = std::env::var_os("LES_ENGINEERING_READERS")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("LES_RUNTIME_HOME")
                    .map(|path| PathBuf::from(path).join("native/engineering-readers"))
            })
            .ok_or(UNAVAILABLE)?;
        if !root.is_absolute() || !root.is_dir() {
            return Err(UNAVAILABLE);
        }
        let root = root.canonicalize().map_err(|_| ASSET)?;
        let bytes = bounded(&root.join("reader-receipt.json"), 128 * 1024).map_err(|_| ASSET)?;
        let receipt: Value = serde_json::from_slice(&bytes).map_err(|_| ASSET)?;
        let files = receipt["files"].as_object().ok_or(ASSET)?;
        if receipt["schema"] != "les.engineering-reader-assets.v1"
            || files.is_empty()
            || files.len() > 512
        {
            return Err(ASSET);
        }
        for (name, expected) in files {
            let relative = Path::new(name);
            if !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(ASSET);
            }
            let path = root.join(relative);
            if path.canonicalize().ok().as_deref() != Some(path.as_path()) {
                return Err(ASSET);
            }
            let file = bounded(&path, MAX_OUTPUT).map_err(|_| ASSET)?;
            if *expected != les_core::sha256_hex(&file) {
                return Err(ASSET);
            }
        }
        let dwg = [
            node_name(),
            "read-data.mjs",
            "node_modules/@mlightcad/libredwg-web/package.json",
            "node_modules/@mlightcad/libredwg-web/dist/libredwg-web.js",
            "node_modules/@mlightcad/libredwg-web/wasm/libredwg-web.wasm",
        ]
        .iter()
        .all(|name| files.contains_key(*name));
        let rvt = files.contains_key(rvt_name());
        let ifc = [
            node_name(),
            "ifc-data.cjs",
            "node_modules/web-ifc/package.json",
            "node_modules/web-ifc/web-ifc-api-node.js",
            "node_modules/web-ifc/web-ifc-node.wasm",
        ]
        .iter()
        .all(|name| files.contains_key(*name));
        Ok(Self {
            root: reader_directory(root)?,
            revision: les_core::sha256_hex(&bytes),
            dwg,
            rvt,
            ifc,
        })
    }
}

fn reader_directory(root: PathBuf) -> Result<PathBuf> {
    // Node's Windows module resolver expects an ordinary drive or UNC cwd.
    #[cfg(windows)]
    {
        let path = root.to_str().ok_or(ASSET)?;
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(drive) = path.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(drive));
        }
    }
    Ok(root)
}

fn node_name() -> &'static str {
    if cfg!(windows) {
        "node.exe"
    } else {
        "node"
    }
}

fn rvt_name() -> &'static str {
    if cfg!(windows) {
        "ifccs-rvt-metadata.exe"
    } else {
        "ifccs-rvt-metadata"
    }
}

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| FAILED)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| FAILED)?;
    if bytes.len() as u64 > limit {
        return Err(LIMIT);
    }
    Ok(bytes)
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&bounded(path, MAX_OUTPUT)?).map_err(|_| FAILED)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(input: &Value) -> Result<Self> {
        let root = input["owned_state"]
            .as_str()
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("LES_STATE_ROOT").map(PathBuf::from))
            .ok_or(FAILED)?;
        if !root.is_absolute() || !root.is_dir() {
            return Err(FAILED);
        }
        let root = root.canonicalize().map_err(|_| FAILED)?;
        let storage = root.join("storage");
        let parent = storage.join("engineering-tmp");
        for dir in [&storage, &parent] {
            match fs::create_dir(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(FAILED),
            }
            if !dir.is_dir() || dir.canonicalize().ok().as_deref() != Some(dir.as_path()) {
                return Err(FAILED);
            }
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FAILED)?
            .as_nanos();
        let path = parent.join(format!(
            "{}-{time}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).map_err(|_| FAILED)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for filename in [
            "request.json",
            "inventory.json",
            "records.jsonl",
            "records.summary.json",
            "dwg.json",
            "ifc.json",
        ] {
            let _ = fs::remove_file(self.0.join(filename));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn run(command: &mut Command, deadline: Instant, output: &Path) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    les_core::prepare_child(command);
    let mut child = command.spawn().map_err(|_| FAILED)?;
    // The parent format worker also supervises the process tree.
    let result = (|| loop {
        if let Some(status) = child.try_wait().map_err(|_| FAILED)? {
            return if status.success() && output.is_file() {
                Ok(())
            } else if status.code() == Some(2)
                && output.file_name().is_some_and(|s| s == "ifc.json")
            {
                Err(LIMIT)
            } else {
                Err(FAILED)
            };
        }
        if fs::metadata(output).is_ok_and(|meta| meta.len() > MAX_OUTPUT) {
            return Err(LIMIT);
        }
        if Instant::now() >= deadline {
            return Err("WORKER_TIMEOUT");
        }
        thread::sleep(Duration::from_millis(25));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn rvt_command(
    readers: &Readers,
    original: &Path,
    request: &Path,
    output: &Path,
    deadline: Instant,
) -> Result<()> {
    run(
        Command::new(readers.root.join(rvt_name()))
            .current_dir(&readers.root)
            .arg("--extract")
            .arg(original)
            .arg(request)
            .arg(output),
        deadline,
        output,
    )
}

fn rvt_details(
    readers: &Readers,
    original: &Path,
    request: &Path,
    output: &Path,
    schema_hash: &Value,
    objects: &mut [Value],
    deadline: Instant,
) -> Result<bool> {
    let referenced: BTreeSet<_> = objects
        .iter()
        .flat_map(|o| o["model_context"]["types"].as_array().into_iter().flatten())
        .filter_map(|r| {
            r["native_id"]
                .as_str()
                .and_then(|id| id.parse::<u64>().ok())
        })
        .collect();
    let mut wanted = BTreeMap::new();
    for (index, object) in objects.iter().enumerate() {
        let id = object["native_id"]
            .as_str()
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or(FAILED)?;
        if object["name"].is_null()
            && (object["section"] == "levels"
                || object["section"] == "types" && referenced.contains(&id))
        {
            wanted.insert(id, index);
        }
    }
    if wanted.is_empty() {
        return Ok(false);
    }
    let ids: Vec<_> = wanted.keys().copied().take(512).collect();
    let mut incomplete = wanted.len() > ids.len();
    let deadline = deadline.min(Instant::now() + Duration::from_secs(25));
    let mut offset = 0usize;
    loop {
        let page = json!({"mode":"records","projection":"graphs","element_ids":ids,
            "offset":offset,"max_records":512,"page_bytes":8*1024*1024});
        fs::write(request, page.to_string()).map_err(|_| FAILED)?;
        rvt_command(readers, original, request, output, deadline)?;
        let summary = read_json(&output.with_extension("summary.json"))?;
        let size = fs::metadata(output).map_err(|_| FAILED)?.len();
        if size > MAX_OUTPUT
            || summary["schema_version"] != 2
            || &summary["schema_sha256"] != schema_hash
            || summary["pagination"]["offset"] != offset
            || summary["output_bytes"] != size
        {
            return Err(FAILED);
        }
        let mut returned = 0usize;
        for line in BufReader::new(File::open(output).map_err(|_| FAILED)?).lines() {
            let row: Value =
                serde_json::from_str(&line.map_err(|_| FAILED)?).map_err(|_| FAILED)?;
            let id = row["identity"]["element_id"].as_u64().ok_or(FAILED)?;
            let object = &mut objects[*wanted.get(&id).ok_or(FAILED)?];
            if row["class_name"] == object["category"] {
                if let Some(name) = rvt_saved_label(&row, object["section"].as_str().ok_or(FAILED)?)
                {
                    object["name"] = json!(name);
                }
            }
            returned += 1;
        }
        if summary["record_count"] != returned || summary["pagination"]["returned"] != returned {
            return Err(FAILED);
        }
        incomplete |= summary["status"] != "extracted_requires_review";
        if summary["pagination"]["next_offset"].is_null() || !summary["error"].is_null() {
            return Ok(incomplete);
        }
        offset += returned;
        if returned == 0 || offset >= ids.len() || summary["pagination"]["next_offset"] != offset {
            return Err(FAILED);
        }
    }
}

fn rvt(
    readers: &Readers,
    original: &Path,
    scratch: &Path,
    deadline: Instant,
) -> Result<(Vec<Value>, Value)> {
    let request = scratch.join("request.json");
    let inventory = scratch.join("inventory.json");
    fs::write(&request, br#"{"mode":"inventory"}"#).map_err(|_| FAILED)?;
    rvt_command(readers, original, &request, &inventory, deadline)?;
    let inventory = read_json(&inventory)?;
    let classes = rvt_classes(&inventory)?;
    let output = scratch.join("records.jsonl");
    let mut objects = Vec::new();
    let mut offset = 0u64;
    let mut omissions = 0u64;
    let mut incomplete = false;
    loop {
        let page = json!({"mode":"records","projection":"compact","classes":classes.keys().collect::<Vec<_>>(),
            "offset":offset,"max_records":2000,"page_bytes":16*1024*1024});
        fs::write(&request, page.to_string()).map_err(|_| FAILED)?;
        rvt_command(readers, original, &request, &output, deadline)?;
        let summary = read_json(&output.with_extension("summary.json"))?;
        let size = fs::metadata(&output).map_err(|_| FAILED)?.len();
        if size > MAX_OUTPUT {
            return Err(LIMIT);
        }
        if summary["schema_version"] != 2
            || summary["schema_sha256"] != inventory["schema_sha256"]
            || summary["pagination"]["offset"] != offset
            || summary["output_bytes"] != size
        {
            return Err(FAILED);
        }
        let mut returned = 0;
        for line in BufReader::new(File::open(&output).map_err(|_| FAILED)?).lines() {
            let row: Value =
                serde_json::from_str(&line.map_err(|_| FAILED)?).map_err(|_| FAILED)?;
            returned += 1;
            if let Some(object) =
                rvt_object(&row, &classes, &summary["summary"]["parameter_definitions"])?
            {
                objects.push(object);
                if objects.len() > MAX_OBJECTS {
                    return Err(LIMIT);
                }
            } else {
                omissions += 1;
            }
        }
        if summary["record_count"] != returned || summary["pagination"]["returned"] != returned {
            return Err(FAILED);
        }
        incomplete |= summary["status"] != "extracted_requires_review";
        if !summary["error"].is_null() || summary["pagination"]["next_offset"].is_null() {
            break;
        }
        let next = offset + returned;
        if returned == 0 || summary["pagination"]["next_offset"] != next {
            return Err(FAILED);
        }
        if next > MAX_OBJECTS as u64 {
            return Err(LIMIT);
        }
        offset = next;
    }
    incomplete |= rvt_details(
        readers,
        original,
        &request,
        &output,
        &inventory["schema_sha256"],
        &mut objects,
        deadline,
    )
    .unwrap_or(true);
    rvt_context(&mut objects);
    Ok((
        objects,
        json!({"skipped_records":omissions,"reader_reported_gaps":incomplete}),
    ))
}

pub(crate) fn read(input: &Value, original: &Path, format: &str) -> Result<Value> {
    let readers = READERS
        .get_or_init(Readers::load)
        .as_ref()
        .map_err(|code| *code)?;
    if !match format {
        "dwg" => readers.dwg,
        "rvt" => readers.rvt,
        "ifc" => readers.ifc,
        _ => false,
    } {
        return Err(UNAVAILABLE);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let _original = options.open(original).map_err(|_| "DOCUMENT_READ_FAILED")?;
    let scratch = Scratch::new(input)?;
    let deadline = Instant::now() + Duration::from_secs(110);
    let (engine, objects, information, limitation) = if format == "dwg" {
        let output = scratch.0.join("dwg.json");
        run(
            Command::new(readers.root.join(node_name()))
                .current_dir(&readers.root)
                .arg(readers.root.join("read-data.mjs"))
                .arg(original)
                .arg(&output),
            deadline,
            &output,
        )?;
        let value = read_json(&output)?;
        (
            "LibreDWG/WASM",
            dwg_objects(&value)?,
            json!({"drawing_units":value["units"]}),
            "Внешние ссылки и вычисленные таблицы DWG могут отсутствовать.",
        )
    } else if format == "ifc" {
        let output = scratch.0.join("ifc.json");
        run(
            Command::new(readers.root.join(node_name()))
                .current_dir(&readers.root)
                .arg("--max-old-space-size=1024")
                .arg(readers.root.join("ifc-data.cjs"))
                .arg(original)
                .arg(&output),
            deadline,
            &output,
        )?;
        let mut value = read_json(&output)?;
        if value["schema"] != "les.ifc-data.v1"
            || !matches!(
                value["ifc_schema"].as_str(),
                Some("IFC2X3" | "IFC4" | "IFC4X3")
            )
            || !value["unit_count"].is_u64()
        {
            return Err(FAILED);
        }
        let Value::Array(objects) = value["objects"].take() else {
            return Err(FAILED);
        };
        (
            "WebIFC/WASM",
            objects,
            json!({"ifc_schema":value["ifc_schema"],"project_unit_count":value["unit_count"]}),
            "Прочитаны сохранённые свойства и связи IFC. Геометрия и расчётные величины не вычислялись.",
        )
    } else {
        let (objects, information) = rvt(readers, original, &scratch.0, deadline)?;
        ("rvt-rs/native", objects, information,
            "Прочитаны сохранённые данные выбранных классов. Виды и спецификации Revit не пересчитывались.")
    };
    if objects.len() > MAX_OBJECTS {
        return Err(LIMIT);
    }
    Ok(
        json!({"schema":"les.engineering-reader.v1","objects":objects,"object_count":objects.len(),
        "reader":{"engine":engine,"revision":readers.revision,"format":format},
        "coverage":{"status":"partial","limitations":[limitation],"details":information}}),
    )
}
