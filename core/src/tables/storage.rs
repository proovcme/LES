//! Interoperable Snappy Parquet. Publication never exposes an unfinished footer.
use super::{field, output, Result, INVALID, LIMIT, MAX_ROWS, SCHEMA};
use parquet::{
    basic::{Compression, LogicalType, Repetition, Type as Physical},
    data_type::{BoolType, ByteArray, ByteArrayType, DoubleType, Int64Type},
    file::{
        properties::WriterProperties, reader::FileReader, serialized_reader::SerializedFileReader,
        writer::SerializedFileWriter,
    },
    record::{Field, Row},
    schema::types::Type,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
const READ: &str = "PARQUET_READ_FAILED";
const WRITE: &str = "PARQUET_WRITE_FAILED";
const FILE_LIMIT: u64 = 256 * 1024 * 1024;

fn failure(error: &(dyn std::error::Error + 'static), fallback: &'static str) -> &'static str {
    let mut cause = Some(error);
    let mut code = fallback;
    let mut kind = None;
    let mut os = None;
    for _ in 0..12 {
        let Some(current) = cause else {
            break;
        };
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            kind = Some(format!("{:?}", io.kind()));
            os = io.raw_os_error();
            if io.kind() == std::io::ErrorKind::StorageFull {
                code = "STORAGE_FULL";
            }
            break;
        }
        cause = current.source();
    }
    eprintln!(
        "{}",
        json!({"event":"parquet_io_failed","code":code,"io_kind":kind,"os_code":os,
        "diagnostic":error.to_string().chars().take(2000).collect::<String>()})
    );
    code
}

fn path(request: &Value) -> Result<&Path> {
    let path = Path::new(field(request, "path"));
    if !path.is_absolute()
        || path
            .extension()
            .is_none_or(|s| !s.eq_ignore_ascii_case("parquet"))
    {
        return Err("PARQUET_PATH_INVALID");
    }
    Ok(path)
}
fn regular(path: &Path, missing: bool) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) if missing && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("PARQUET_SOURCE_UNAVAILABLE"),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("PARQUET_PATH_INVALID");
        }
    }
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("PARQUET_PATH_INVALID");
    }
    Ok(())
}
pub(super) fn rows(request: &Value) -> Result<Vec<Value>> {
    if let Some(rows) = request["rows"].as_array() {
        if rows.len() > MAX_ROWS {
            return Err(LIMIT);
        }
        return Ok(rows.clone());
    }
    let source = Path::new(field(request, "rows_path"));
    if !source.is_absolute() {
        return Err(INVALID);
    }
    regular(source, false)?;
    let file = File::open(source).map_err(|error| failure(&error, READ))?;
    if file
        .metadata()
        .map_err(|error| failure(&error, READ))?
        .len()
        > 64 * 1024 * 1024
    {
        return Err(LIMIT);
    }
    let mut out = Vec::new();
    let mut total = 0;
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|error| failure(&error, READ))?;
        total += line.len();
        if total > 64 * 1024 * 1024 || line.len() > 8 * 1024 * 1024 || out.len() >= MAX_ROWS {
            return Err(LIMIT);
        }
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_slice(&line).map_err(|_| INVALID)?);
    }
    Ok(out)
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Bool,
    Int,
    UInt,
    Float,
    Text,
}
fn schema(rows: &[Value]) -> Result<Vec<(String, Kind)>> {
    let mut keys = BTreeSet::new();
    for row in rows {
        let row = row.as_object().ok_or(INVALID)?;
        keys.extend(row.keys().cloned());
    }
    if keys.len() > 512 {
        return Err(LIMIT);
    }
    let mut out = Vec::new();
    for key in keys {
        if key.is_empty() || key.len() > 65536 {
            return Err(INVALID);
        }
        let numeric = SCHEMA["numeric"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == &key);
        let mut kind = None;
        for row in rows {
            let value = &row[&key];
            if value.is_null() {
                continue;
            }
            let next = match value {
                Value::Bool(_) => Kind::Bool,
                Value::Number(n) if n.is_i64() => Kind::Int,
                Value::Number(n) if n.is_u64() => Kind::UInt,
                Value::Number(n) if n.as_f64().is_some_and(|n| n.is_finite()) => Kind::Float,
                Value::String(_) if numeric => Kind::Float,
                Value::String(_) => Kind::Text,
                _ => return Err("PARQUET_VALUE_UNSUPPORTED"),
            };
            kind = match kind {
                None => Some(next),
                Some(old) if old == next => Some(old),
                Some(Kind::Float | Kind::Int) if matches!(next, Kind::Float | Kind::Int) => {
                    Some(Kind::Float)
                }
                _ => return Err("PARQUET_VALUE_UNSUPPORTED"),
            };
        }
        let kind = kind.unwrap_or(if numeric { Kind::Float } else { Kind::Text });
        if kind == Kind::Float
            && rows.iter().any(|row| {
                row[&key]
                    .as_i64()
                    .is_some_and(|n| n.unsigned_abs() > 9_007_199_254_740_992)
            })
        {
            return Err("PARQUET_NUMBER_PRECISION");
        }
        out.push((key, kind));
    }
    Ok(out)
}
struct Pending(PathBuf);
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn pending(path: &Path) -> Result<(Pending, File)> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| failure(&error, WRITE))?;
    let hex = random
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    let temp = path.with_file_name(format!(".les-parquet-{hex}.pending"));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| failure(&error, WRITE))?;
    Ok((Pending(temp), file))
}
pub(super) fn write(request: &Value) -> Result<Value> {
    let path = path(request)?;
    regular(path, true)?;
    let rows = rows(request)?;
    if rows.is_empty() {
        return Ok(json!({"rows":0,"owner":"rust.tables.parquet"}));
    }
    let columns = schema(&rows)?;
    let fields = columns
        .iter()
        .map(|(key, kind)| {
            let physical = match kind {
                Kind::Bool => Physical::BOOLEAN,
                Kind::Int | Kind::UInt => Physical::INT64,
                Kind::Float => Physical::DOUBLE,
                Kind::Text => Physical::BYTE_ARRAY,
            };
            let mut builder =
                Type::primitive_type_builder(key, physical).with_repetition(Repetition::OPTIONAL);
            if *kind == Kind::Text {
                builder = builder.with_logical_type(Some(LogicalType::String));
            }
            if *kind == Kind::UInt {
                builder = builder.with_logical_type(Some(LogicalType::integer(64, false)));
            }
            builder
                .build()
                .map(Arc::new)
                .map_err(|error| failure(&error, WRITE))
        })
        .collect::<Result<Vec<_>>>()?;
    let schema = Arc::new(
        Type::group_type_builder("les_table")
            .with_fields(fields)
            .build()
            .map_err(|error| failure(&error, WRITE))?,
    );
    let (pending, file) = pending(path)?;
    let sync = file.try_clone().map_err(|error| failure(&error, WRITE))?;
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_created_by("LES Rust tables v1".into())
            .build(),
    );
    let mut writer =
        SerializedFileWriter::new(file, schema, props).map_err(|error| failure(&error, WRITE))?;
    for batch in rows.chunks(1024) {
        let mut group = writer
            .next_row_group()
            .map_err(|error| failure(&error, WRITE))?;
        for (key, kind) in &columns {
            let mut column = group
                .next_column()
                .map_err(|error| failure(&error, WRITE))?
                .ok_or(WRITE)?;
            let values = batch.iter().map(|row| &row[key]).collect::<Vec<_>>();
            let defs = values
                .iter()
                .map(|v| i16::from(!v.is_null()))
                .collect::<Vec<_>>();
            match kind {
                Kind::Bool => {
                    let values = values
                        .iter()
                        .filter_map(|v| v.as_bool())
                        .collect::<Vec<_>>();
                    column
                        .typed::<BoolType>()
                        .write_batch(&values, Some(&defs), None)
                        .map_err(|error| failure(&error, WRITE))?;
                }
                Kind::Int | Kind::UInt => {
                    let values = values
                        .iter()
                        .filter(|v| !v.is_null())
                        .map(|v| {
                            if *kind == Kind::UInt {
                                v.as_u64().map(|v| v as i64)
                            } else {
                                v.as_i64()
                            }
                            .ok_or("PARQUET_VALUE_UNSUPPORTED")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    column
                        .typed::<Int64Type>()
                        .write_batch(&values, Some(&defs), None)
                        .map_err(|error| failure(&error, WRITE))?;
                }
                Kind::Float => {
                    let mut defs = Vec::new();
                    let mut floats = Vec::new();
                    for value in values {
                        let value = value
                            .as_f64()
                            .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
                            .filter(|v| v.is_finite());
                        defs.push(i16::from(value.is_some()));
                        if let Some(v) = value {
                            floats.push(v);
                        }
                    }
                    column
                        .typed::<DoubleType>()
                        .write_batch(&floats, Some(&defs), None)
                        .map_err(|error| failure(&error, WRITE))?;
                }
                Kind::Text => {
                    let values = values
                        .iter()
                        .filter(|v| !v.is_null())
                        .map(|v| {
                            v.as_str()
                                .map(ByteArray::from)
                                .ok_or("PARQUET_VALUE_UNSUPPORTED")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    column
                        .typed::<ByteArrayType>()
                        .write_batch(&values, Some(&defs), None)
                        .map_err(|error| failure(&error, WRITE))?;
                }
            }
            column.close().map_err(|error| failure(&error, WRITE))?;
        }
        group.close().map_err(|error| failure(&error, WRITE))?;
    }
    writer.close().map_err(|error| failure(&error, WRITE))?;
    sync.sync_all().map_err(|error| failure(&error, WRITE))?;
    drop(sync);
    let checksum = checksum(&pending.0)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::rename(&pending.0, path) {
            Ok(()) => break,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Err(WRITE),
        }
    }
    #[cfg(unix)]
    File::open(path.parent().ok_or(WRITE)?)
        .and_then(|f| f.sync_all())
        .map_err(|error| failure(&error, WRITE))?;
    Ok(
        json!({"rows":rows.len(),"owner":"rust.tables.parquet","compression":"snappy","sha256":checksum}),
    )
}

pub(super) fn checksum(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| failure(&error, READ))?;
    let size = file
        .metadata()
        .map_err(|error| failure(&error, READ))?
        .len();
    if size > FILE_LIMIT {
        return Err(LIMIT);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| failure(&error, READ))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > FILE_LIMIT {
            return Err(LIMIT);
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn open(path: &Path) -> Result<(SerializedFileReader<File>, String)> {
    regular(path, false)?;
    let mut file = File::open(path).map_err(|error| failure(&error, READ))?;
    let size = file
        .metadata()
        .map_err(|error| failure(&error, READ))?
        .len();
    if !(12..=FILE_LIMIT).contains(&size) {
        return Err(LIMIT);
    }
    file.seek(SeekFrom::End(-8))
        .map_err(|error| failure(&error, READ))?;
    let mut footer = [0; 8];
    file.read_exact(&mut footer)
        .map_err(|error| failure(&error, READ))?;
    let metadata = u32::from_le_bytes(footer[..4].try_into().unwrap()) as u64;
    if &footer[4..] != b"PAR1" || metadata > 8 * 1024 * 1024 || metadata + 8 > size {
        return Err(READ);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| failure(&error, READ))?;
    let mut digest = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let n = file
            .read(&mut block)
            .map_err(|error| failure(&error, READ))?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| failure(&error, READ))?;
    let reader = SerializedFileReader::new(file).map_err(|error| failure(&error, READ))?;
    let meta = reader.metadata();
    if meta.file_metadata().num_rows() < 0
        || meta.file_metadata().num_rows() as usize > MAX_ROWS
        || meta.file_metadata().schema_descr().num_columns() > 512
        || meta.num_row_groups() > 10_000
    {
        return Err(LIMIT);
    }
    let mut expanded = 0_i64;
    for group in meta.row_groups() {
        if group.columns().iter().any(|c| {
            !matches!(
                c.compression(),
                Compression::SNAPPY | Compression::UNCOMPRESSED
            )
        }) {
            return Err("PARQUET_CODEC_UNSUPPORTED");
        }
        let n = group.total_byte_size();
        if !(0..=64 * 1024 * 1024).contains(&n) {
            return Err(LIMIT);
        }
        expanded = expanded.checked_add(n).ok_or(LIMIT)?;
        if expanded > 512 * 1024 * 1024 {
            return Err(LIMIT);
        }
    }
    Ok((reader, format!("{:x}", digest.finalize())))
}
fn value(field: &Field) -> Result<Value> {
    Ok(match field {
        Field::Null => Value::Null,
        Field::Bool(v) => json!(v),
        Field::Byte(v) => json!(v),
        Field::Short(v) => json!(v),
        Field::Int(v) => json!(v),
        Field::Long(v) => json!(v),
        Field::UByte(v) => json!(v),
        Field::UShort(v) => json!(v),
        Field::UInt(v) => json!(v),
        Field::ULong(v) => json!(v),
        Field::Float(v) => {
            if v.is_finite() {
                json!(v)
            } else {
                Value::Null
            }
        }
        Field::Double(v) => {
            if v.is_finite() {
                json!(v)
            } else {
                Value::Null
            }
        }
        Field::Str(v) => json!(v),
        Field::Group(row) => row_value(row)?,
        _ => return Err("PARQUET_VALUE_UNSUPPORTED"),
    })
}
pub(super) fn row_value(row: &Row) -> Result<Value> {
    let mut out = Map::new();
    for (key, field) in row.get_column_iter() {
        out.insert(key.clone(), value(field)?);
    }
    Ok(Value::Object(out))
}
pub(super) fn read(request: &Value) -> Result<Value> {
    let (reader, hash) = open(path(request)?)?;
    if request
        .get("source_sha256")
        .is_some_and(|v| v.as_str() != Some(&hash))
    {
        return Err("PARQUET_SOURCE_CHANGED");
    }
    let offset = request
        .get("offset")
        .map_or(Some(0), Value::as_u64)
        .ok_or(INVALID)? as usize;
    let limit = request
        .get("limit")
        .map_or(Some(2000), Value::as_u64)
        .filter(|n| (1..=20_000).contains(n))
        .ok_or(INVALID)? as usize;
    let total = reader.metadata().file_metadata().num_rows() as usize;
    if offset > total {
        return Err(INVALID);
    }
    let mut rows = Vec::new();
    let mut bytes = 0;
    for row in reader
        .get_row_iter(None)
        .map_err(|error| failure(&error, READ))?
        .skip(offset)
        .take(limit)
    {
        let row = row_value(&row.map_err(|error| failure(&error, READ))?)?;
        bytes += serde_json::to_vec(&row)
            .map_err(|error| failure(&error, READ))?
            .len();
        if bytes > 24 * 1024 * 1024 {
            return Err(LIMIT);
        }
        rows.push(row);
    }
    let next = offset + rows.len();
    output(
        json!({"rows":rows,"row_count":total,"offset":offset,"next_offset":if next<total{Some(next)}else{None},"complete":next==total,"source_sha256":hash,"owner":"rust.tables.parquet"}),
    )
}
