//! Read-only, bounded XLSX/XLSM snapshots for source evidence and attachments.
use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx, XlsxFormulaMetadata};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{self, Cursor, Read},
    path::Path,
};
use zip::ZipArchive;

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ROWS: u32 = 20_000;
const MAX_COLUMNS: u32 = 512;
const MAX_CELLS: usize = 300_000;
const MAX_PAYLOAD: usize = 30 * 1024 * 1024;
pub(super) const COMPAT: &str = "WORKBOOK_COMPATIBILITY";
pub(super) type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "WORKBOOK_INVALID";

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| "WORKBOOK_UNAVAILABLE")?;
    let metadata = file.metadata().map_err(|_| "WORKBOOK_UNAVAILABLE")?;
    if !metadata.is_file() {
        return Err("WORKBOOK_UNAVAILABLE");
    }
    if metadata.len() > MAX_BYTES {
        return Err("WORKBOOK_FILE_LIMIT");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "WORKBOOK_UNAVAILABLE")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("WORKBOOK_FILE_LIMIT");
    }
    Ok(bytes)
}

fn validate_archive(bytes: &[u8]) -> Result<()> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| INVALID)?;
    if archive.len() > 10_000 {
        return Err("WORKBOOK_ARCHIVE_LIMIT");
    }
    let mut remaining = MAX_BYTES;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| INVALID)?;
        let declared = entry.size();
        if declared > remaining {
            return Err("WORKBOOK_ARCHIVE_LIMIT");
        }
        // Check actual expansion and CRC before Calamine reads the same immutable bytes.
        let expanded =
            io::copy(&mut entry.take(declared + 1), &mut io::sink()).map_err(|_| INVALID)?;
        if expanded != declared {
            return Err(INVALID);
        }
        remaining -= expanded;
    }
    Ok(())
}

fn address((row, column): (u32, u32)) -> String {
    let mut col = column + 1;
    let mut letters = Vec::with_capacity(3);
    while col > 0 {
        col -= 1;
        letters.push(char::from(b'A' + (col % 26) as u8));
        col /= 26;
    }
    let column: String = letters.into_iter().rev().collect();
    format!("{column}{}", row + 1)
}

pub(super) fn dimensions((row, col): (u32, u32)) -> Result<()> {
    if row >= MAX_ROWS || col >= MAX_COLUMNS || ((row + 1) * (col + 1)) as usize > MAX_CELLS {
        return Err("WORKBOOK_SHEET_LIMIT");
    }
    Ok(())
}

pub(super) fn verified(
    request: &Value,
    read: impl FnOnce(&[u8], String) -> Result<Value>,
) -> Result<Value> {
    let path = Path::new(request["path"].as_str().ok_or(INVALID)?);
    let bytes = read_file(path)?;
    validate_archive(&bytes)?;
    let digest = Sha256::digest(&bytes);
    let result = read(&bytes, format!("{digest:x}"))?;
    drop(bytes);
    if Sha256::digest(read_file(path).map_err(|_| "WORKBOOK_CHANGED")?) != digest {
        return Err("WORKBOOK_CHANGED");
    }
    Ok(result)
}

fn cell_value(value: DataRef<'_>) -> Result<Value> {
    Ok(match value {
        DataRef::Empty => Value::Null,
        DataRef::Int(n) => json!({"kind":"number","value":n.to_string()}),
        DataRef::Float(n) => {
            if !n.is_finite() {
                return Err("WORKBOOK_NONFINITE");
            }
            // Calamine exposes XLSX numbers as f64. Preserve literal large
            // integers through the old reader until we can read their lexemes.
            if n.abs() >= 9_007_199_254_740_992.0 {
                return Err(COMPAT);
            }
            json!({"kind":"number","value":n.to_string()})
        }
        DataRef::Bool(value) => json!({"kind":"boolean","value":value}),
        DataRef::Error(value) => json!({"kind":"error","value":value.to_string()}),
        DataRef::String(ref text) if text.is_empty() => Value::Null,
        DataRef::SharedString("") => Value::Null,
        DataRef::String(text) => json!({"kind":"text","value":text}),
        DataRef::SharedString(text) => json!({"kind":"text","value":text}),
        // Retain the existing epoch, date/time and duration formatting until
        // tested against both Excel calendar systems and custom number formats.
        DataRef::DateTime(_) | DataRef::DateTimeIso(_) | DataRef::DurationIso(_) => {
            return Err(COMPAT)
        }
    })
}

fn formula_text(
    formula: XlsxFormulaMetadata,
    position: (u32, u32),
    shared: &mut HashMap<usize, ((u32, u32), String)>,
) -> Result<String> {
    let text = match formula {
        XlsxFormulaMetadata::Normal { formula } => formula,
        XlsxFormulaMetadata::Shared {
            shared_index,
            range: Some(range),
            formula,
        } if range.start == position => {
            shared.insert(shared_index, (position, formula.clone()));
            formula
        }
        XlsxFormulaMetadata::SharedDerived { shared_index } => {
            let (anchor, formula) = shared.get(&shared_index).ok_or(COMPAT)?;
            calamine::expand_shared_formula(formula, *anchor, position).map_err(|_| COMPAT)?
        }
        _ => return Err(COMPAT),
    };
    if text.is_empty() {
        return Err(COMPAT);
    }
    Ok(format!("={text}"))
}

fn read_sheets(bytes: &[u8]) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut book = Xlsx::new(Cursor::new(bytes)).map_err(|_| COMPAT)?;
    let metadata = book.sheets_metadata().to_vec();
    let mut sheets = Vec::new();
    let mut rows = Vec::new();
    let mut total_cells = 0;
    let mut payload_size = 0;
    for (index, sheet) in metadata.into_iter().enumerate() {
        if sheet.typ != SheetType::WorkSheet {
            return Err(COMPAT);
        }
        let name = &sheet.name;
        let merges = book.merge_cells_by_sheet_name(name).map_err(|_| COMPAT)?;
        let mut merged_ranges = Vec::new();
        let mut merged = HashMap::new();
        let mut max_row = 0;
        let mut max_col = 0;
        for region in &merges {
            max_row = max_row.max(region.end.0);
            max_col = max_col.max(region.end.1);
            dimensions((max_row, max_col))?;
            for row in region.start.0..=region.end.0 {
                for col in region.start.1..=region.end.1 {
                    let position = (row, col);
                    if merged.insert(position, position != region.start).is_some() {
                        return Err(COMPAT);
                    }
                }
            }
            let start = address(region.start);
            let end = address(region.end);
            merged_ranges.push(if start == end {
                start
            } else {
                format!("{start}:{end}")
            });
        }
        let mut reader = book.worksheet_cells_reader(name).map_err(|_| COMPAT)?;
        let mut shared = HashMap::new();
        let mut sheet_rows = BTreeMap::<u32, BTreeMap<u32, Value>>::new();
        while let Some(cell) = reader
            .next_cell_with_formula_metadata()
            .map_err(|_| COMPAT)?
        {
            let (row, col) = cell.pos;
            max_row = max_row.max(row);
            max_col = max_col.max(col);
            dimensions((max_row, max_col))?;
            if merged.get(&cell.pos) == Some(&true) {
                continue;
            }
            let value = cell_value(cell.value)?;
            let coordinate = address(cell.pos);
            let payload = match cell.formula {
                Some(formula) => json!({
                    "coordinate":coordinate, "kind":"formula",
                    "formula":formula_text(formula, cell.pos, &mut shared)?,
                    "needs_recalculation":value.is_null() || value["kind"] == "error",
                    "cached":value,
                }),
                None if value.is_null() => continue,
                None => {
                    let mut value = value;
                    value["coordinate"] = json!(coordinate);
                    value
                }
            };
            total_cells += 1;
            if total_cells > MAX_CELLS {
                return Err("WORKBOOK_CELL_LIMIT");
            }
            payload_size += serde_json::to_vec(&payload).map_err(|_| INVALID)?.len() + 1;
            if payload_size > MAX_PAYLOAD {
                return Err(COMPAT);
            }
            if sheet_rows
                .entry(row)
                .or_default()
                .insert(col, payload)
                .is_some()
            {
                return Err(COMPAT);
            }
        }
        for (row, cells) in sheet_rows {
            if rows.len() >= MAX_ROWS as usize {
                return Err("WORKBOOK_CELL_LIMIT");
            }
            let number = row + 1;
            let record = json!({"id":format!("s{}:r{number}", index+1),"sheet":name,
                "row":number,"locator":format!("{name}!R{number}"),"cells":cells.into_values().collect::<Vec<_>>()});
            // Row envelope (name appears twice) and separators count against the ABI budget.
            payload_size += name.len() * 4 + 160;
            if payload_size > MAX_PAYLOAD {
                return Err(COMPAT);
            }
            rows.push(record);
        }
        let sheet = json!({"name":name,"columns":max_col+1,"merged_ranges":merged_ranges,
            "hidden":sheet.visible != SheetVisible::Visible});
        payload_size += serde_json::to_vec(&sheet).map_err(|_| INVALID)?.len() + 1;
        if payload_size > MAX_PAYLOAD {
            return Err(COMPAT);
        }
        sheets.push(sheet);
    }
    Ok((sheets, rows))
}

pub fn snapshot(request: &Value) -> Result<Value> {
    verified(request, |bytes, digest| {
        let (sheets, rows) = read_sheets(bytes)?;
        Ok(json!({"sheets":sheets,"rows":rows,"sha256":digest}))
    })
}
