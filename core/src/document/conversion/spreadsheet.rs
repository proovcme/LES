//! Navigation text retains worksheet identity and physical source row numbers.
use super::{escape, Result, MAX_TEXT};
use calamine::{Data, DataRef, ExcelDateTime, Reader, SheetType, Xls, Xlsx};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Cursor, path::Path};

const INVALID: &str = "SPREADSHEET_READ_FAILED";
const LIMIT: &str = "SPREADSHEET_LIMIT";
struct Sheet {
    name: String,
    rows: BTreeMap<u32, Vec<String>>,
    total: u32,
    columns: usize,
    missing_formula_cache: Option<usize>,
    typed_rows: BTreeMap<u32, Vec<Value>>,
}

fn date(value: &ExcelDateTime) -> Result<String> {
    let number = value.as_f64();
    if !number.is_finite() {
        return Err(INVALID);
    }
    if value.is_duration() {
        return Ok(format!("{} days (Excel duration)", number));
    }
    if !(0.0..2_958_466.0).contains(&number) {
        return Err(INVALID);
    }
    let (y, m, d, h, min, s, ms) = value.to_ymd_hms_milli();
    Ok(format!(
        "{y:04}-{m:02}-{d:02} {h:02}:{min:02}:{s:02}{}",
        if ms == 0 {
            String::new()
        } else {
            format!(".{ms:03}")
        }
    ))
}
fn scalar(cell: &DataRef<'_>) -> Result<String> {
    Ok(match cell {
        DataRef::Empty => String::new(),
        DataRef::String(s) => s.clone(),
        DataRef::SharedString(s) => s.to_string(),
        DataRef::Bool(v) => if *v { "True" } else { "False" }.into(),
        DataRef::Int(v) => v.to_string(),
        DataRef::Float(v) if v.is_finite() => v.to_string(),
        DataRef::DateTime(v) => date(v)?,
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
        DataRef::Error(v) => v.to_string(),
        _ => return Err(INVALID),
    })
}
fn owned(cell: &Data) -> Result<String> {
    match cell {
        Data::Empty => Ok(String::new()),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => Ok(s.clone()),
        Data::Int(v) => Ok(v.to_string()),
        Data::Float(v) if v.is_finite() => Ok(v.to_string()),
        Data::Bool(v) => Ok(if *v { "True" } else { "False" }.into()),
        Data::Error(v) => Ok(v.to_string()),
        Data::DateTime(v) => date(v),
        _ => Err(INVALID),
    }
}
fn dimensions(row: u32, column: u32) -> Result<()> {
    if row >= 1_048_576 || column >= 512 {
        Err(LIMIT)
    } else {
        Ok(())
    }
}
fn typed(cell: &DataRef<'_>) -> Result<Value> {
    Ok(match cell {
        DataRef::Empty => Value::Null,
        DataRef::Int(v) => json!(v),
        DataRef::Float(v) if v.is_finite() => json!(v),
        DataRef::Bool(v) => json!(v),
        _ => json!(scalar(cell)?),
    })
}
fn owned_typed(cell: &Data) -> Result<Value> {
    Ok(match cell {
        Data::Empty => Value::Null,
        Data::Int(v) => json!(v),
        Data::Float(v) if v.is_finite() => json!(v),
        Data::Bool(v) => json!(v),
        _ => json!(owned(cell)?),
    })
}
fn xlsx(bytes: &[u8], limit: u32, capture: bool) -> Result<Vec<Sheet>> {
    super::archive(bytes)?;
    let mut book = Xlsx::new(Cursor::new(bytes)).map_err(|_| INVALID)?;
    let sheets = book.sheets_metadata().to_vec();
    if sheets.len() > 512 {
        return Err(LIMIT);
    }
    let mut output = Vec::new();
    let mut retained = 0;
    for sheet in sheets {
        if sheet.typ != SheetType::WorkSheet {
            continue;
        }
        let mut reader = book
            .worksheet_cells_reader(&sheet.name)
            .map_err(|_| INVALID)?;
        let (last_row, last_col) = reader.dimensions().end;
        dimensions(last_row, last_col)?;
        let mut rows: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        let mut previous = None;
        let mut missing_formula_cache = 0;
        let mut typed_rows: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
        while let Some(cell) = reader
            .next_cell_with_formula_metadata()
            .map_err(|_| INVALID)?
        {
            let (row, col) = cell.pos;
            dimensions(row, col)?;
            if previous.is_some_and(|old| old >= (row, col)) {
                return Err(INVALID);
            }
            previous = Some((row, col));
            if row > limit {
                break;
            }
            if cell.formula.is_some() && matches!(cell.value, DataRef::Empty) {
                missing_formula_cache += 1;
            }
            let text = scalar(&cell.value)?;
            retained += text.len();
            if retained > MAX_TEXT {
                return Err(LIMIT);
            }
            if text.is_empty() {
                continue;
            }
            if capture {
                let values = typed_rows.entry(row).or_default();
                values.resize(values.len().max(col as usize + 1), Value::Null);
                values[col as usize] = typed(&cell.value)?;
            }
            let values = rows.entry(row).or_default();
            values.resize(values.len().max(col as usize + 1), String::new());
            values[col as usize] = text;
        }
        output.push(Sheet {
            name: sheet.name,
            rows,
            total: last_row.saturating_add(1),
            columns: last_col as usize + 1,
            missing_formula_cache: Some(missing_formula_cache),
            typed_rows,
        });
    }
    Ok(output)
}
fn xls(bytes: &[u8], limit: u32, capture: bool) -> Result<Vec<Sheet>> {
    let mut book = Xls::new(Cursor::new(bytes)).map_err(|_| INVALID)?;
    let sheets = book.sheets_metadata().to_vec();
    if sheets.len() > 512 {
        return Err(LIMIT);
    }
    let mut output = Vec::new();
    let mut retained = 0;
    for sheet in sheets {
        if sheet.typ != SheetType::WorkSheet {
            continue;
        }
        let range = book.worksheet_range(&sheet.name).map_err(|_| INVALID)?;
        let (last_row, last_col) = range.end().unwrap_or((0, 0));
        dimensions(last_row, last_col)?;
        let mut rows: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        let mut typed_rows: BTreeMap<u32, Vec<Value>> = BTreeMap::new();
        for (r, c, cell) in range.used_cells() {
            let (start_row, start_col) = range.start().unwrap_or((0, 0));
            let row = start_row + r as u32;
            if row > limit {
                break;
            }
            let col = start_col as usize + c;
            let text = owned(cell)?;
            retained += text.len();
            if retained > MAX_TEXT {
                return Err(LIMIT);
            }
            if text.is_empty() {
                continue;
            }
            if capture {
                let values = typed_rows.entry(row).or_default();
                values.resize(values.len().max(col + 1), Value::Null);
                values[col] = owned_typed(cell)?;
            }
            let values = rows.entry(row).or_default();
            values.resize(values.len().max(col + 1), String::new());
            values[col] = text;
        }
        output.push(Sheet {
            name: sheet.name,
            rows,
            total: last_row.saturating_add(1),
            columns: last_col as usize + 1,
            missing_formula_cache: None,
            typed_rows,
        });
    }
    Ok(output)
}

fn csv(bytes: &[u8], name: &str, request: &Value, limit: u32) -> Result<Vec<Sheet>> {
    let text = if let Some(text) = request["decoded_text"].as_str() {
        if request["decoded_sha256"].as_str() != Some(crate::sha256_hex(bytes).as_str()) {
            return Err("SOURCE_CHANGED_DURING_INDEXING");
        }
        text.to_owned()
    } else {
        super::super::plain_text::decode(bytes).ok_or("DOCUMENT_ENCODING_REQUIRED")?
    };
    if text.len() > MAX_TEXT {
        return Err(LIMIT);
    }
    if text
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
    {
        return Err("DOCUMENT_BINARY_TEXT");
    }
    let delimiter = match request["delimiter"].as_str().unwrap_or(",") {
        "," => ',',
        ";" => ';',
        "\t" => '\t',
        _ => return Err("CSV_DELIMITER_INVALID"),
    };
    let mut rows = BTreeMap::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut closed = false;
    let mut chars = text.chars().peekable();
    let mut count = 0_u32;
    let mut width = None;
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                cell.push(ch);
            }
            continue;
        }
        if ch == '"' && cell.is_empty() && !closed {
            quoted = true;
            continue;
        }
        if ch == delimiter || ch == '\n' || ch == '\r' {
            row.push(std::mem::take(&mut cell));
            closed = false;
            if row.len() > 512 {
                return Err(LIMIT);
            }
            if ch == delimiter {
                continue;
            }
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if row.len() == 1 && row[0].is_empty() {
                row.clear();
                continue;
            }
            if let Some(width) = width {
                if row.len() > width {
                    return Err("CSV_ROW_WIDTH_INVALID");
                }
            } else {
                width = Some(row.len());
            }
            rows.insert(count, std::mem::take(&mut row));
            count += 1;
            if count > limit {
                break;
            }
        } else {
            if closed {
                return Err("CSV_QUOTING_INVALID");
            }
            cell.push(ch);
        }
    }
    if quoted {
        return Err("CSV_QUOTING_INVALID");
    }
    if count <= limit && (!row.is_empty() || !cell.is_empty() || closed) {
        row.push(cell);
        if row.len() > 512 || width.is_some_and(|w| row.len() > w) {
            return Err("CSV_ROW_WIDTH_INVALID");
        }
        rows.insert(count, row);
        count += 1;
    }
    Ok(vec![Sheet {
        name: name.into(),
        rows,
        total: count + u32::from(chars.peek().is_some()),
        columns: width.unwrap_or(0),
        missing_formula_cache: None,
        typed_rows: BTreeMap::new(),
    }])
}

fn setting(request: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<usize> {
    request
        .get(key)
        .map_or(Some(default), Value::as_u64)
        .filter(|n| (min..=max).contains(n))
        .map(|n| n as usize)
        .ok_or("SPREADSHEET_SETTINGS_INVALID")
}
fn line(values: impl Iterator<Item = String>) -> String {
    format!("| {} |", values.collect::<Vec<_>>().join(" | "))
}
fn render(sheet: &Sheet, request: &Value, limit: usize, csv: bool) -> Result<(String, Value)> {
    let Some((&header, head)) = sheet.rows.first_key_value() else {
        return Ok((
            String::new(),
            json!({"name":sheet.name,"empty":true,"missing_formula_cache":sheet.missing_formula_cache}),
        ));
    };
    let rows = sheet
        .rows
        .range(header + 1..)
        .filter(|(_, r)| r.iter().any(|v| !v.trim().is_empty()))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return Ok((
            String::new(),
            json!({"name":sheet.name,"empty":true,"missing_formula_cache":sheet.missing_formula_cache}),
        ));
    }
    let width = sheet.rows.values().map(Vec::len).max().unwrap_or(0);
    let columns = (0..width)
        .filter(|&c| {
            rows.iter()
                .any(|(_, r)| r.get(c).is_some_and(|v| !v.trim().is_empty()))
        })
        .collect::<Vec<_>>();
    if rows.len().saturating_mul(columns.len()) > 300_000 {
        return Err(LIMIT);
    }
    let full = setting(request, "full_table_max_cells", 800, 1, 300_000)?;
    let samples = setting(request, "sample_rows", 8, 1, 2000)?;
    let projected = rows.len() * columns.len() > full;
    let truncated = sheet.total as usize > limit + 1;
    let mut parts = vec![format!("## Лист: {}", sheet.name)];
    if projected {
        parts.extend(["Тип: spreadsheet_navigation_projection".into(),format!("Размер прочитанного окна: строк {}, колонок {}",rows.len(),columns.len()),"Назначение: навигация по книге; точные строки и расчеты читать из исходного файла табличным reader/tool.".into()]);
    }
    if truncated {
        parts.push(format!("Примечание: прочитано первые {limit} строк после заголовка; исходный лист может быть длиннее."));
    }
    parts.push(
        if csv {
            "CSV: исходные текстовые значения; формулы не выполнялись."
        } else {
            "Значения: сохранённый результат формул; пересчёт не выполнялся."
        }
        .into(),
    );
    if projected {
        parts.push("### Колонки".into());
        parts.push(
            columns
                .iter()
                .map(|&c| {
                    head.get(c)
                        .filter(|v| !v.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("Колонка {}", c + 1))
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        parts.push("### Профили колонок".into());
        for &c in columns
            .iter()
            .take(setting(request, "profile_columns", 60, 1, 512)?)
        {
            let mut values = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut count = 0;
            for (_, row) in &rows {
                if let Some(v) = row.get(c).filter(|v| !v.is_empty()) {
                    count += 1;
                    if values.len() < setting(request, "profile_values", 12, 1, 200)?
                        && seen.insert(v)
                    {
                        values.push(
                            v.split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                                .chars()
                                .take(120)
                                .collect::<String>(),
                        );
                    }
                }
            }
            parts.push(format!(
                "- {}: заполнено {count} · примеры: {}",
                head.get(c).map_or("", String::as_str),
                values.join("; ")
            ));
        }
        parts.push("### Образец строк".into());
    }
    parts.push(line(
        std::iter::once("Строка источника".into()).chain(
            columns
                .iter()
                .map(|&c| escape(head.get(c).map_or("", String::as_str))),
        ),
    ));
    parts.push(line((0..columns.len() + 1).map(|_| "---".into())));
    for (r, values) in rows
        .iter()
        .take(if projected { samples } else { rows.len() })
    {
        parts.push(line(
            std::iter::once((**r + 1).to_string()).chain(
                columns
                    .iter()
                    .map(|&c| escape(values.get(c).map_or("", String::as_str))),
            ),
        ));
    }
    Ok((
        parts.join("\n"),
        json!({"name":sheet.name,"rows_in_window":rows.len(),"last_source_row":if csv && truncated {None} else {Some(sheet.total)},"columns":sheet.columns,"projected":projected,"truncated":truncated,"header_row":header+1,"missing_formula_cache":sheet.missing_formula_cache}),
    ))
}
pub(super) fn read(bytes: &[u8], suffix: &str, path: &Path, request: &Value) -> Result<Value> {
    let limit = setting(request, "read_rows", 2000, 1, 20_000)?;
    let capture = match request["view"].as_str().unwrap_or("markdown") {
        "markdown" => false,
        "tables" => true,
        _ => return Err("SPREADSHEET_SETTINGS_INVALID"),
    };
    let sheets = match suffix {
        "xlsx" | "xlsm" if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) => {
            xls(bytes, limit as u32, capture)?
        }
        "xls" if bytes.starts_with(b"PK\x03\x04") => xlsx(bytes, limit as u32, capture)?,
        "xlsx" | "xlsm" => xlsx(bytes, limit as u32, capture)?,
        "xls" => xls(bytes, limit as u32, capture)?,
        "csv" => csv(
            bytes,
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("CSV"),
            request,
            limit as u32,
        )?,
        _ => return Err(INVALID),
    };
    if capture {
        let mut tables = Vec::new();
        for mut sheet in sheets {
            // Structured ingestion must never publish only a navigation window.
            if sheet.total as usize > limit + 1 {
                return Err("SPREADSHEET_LIMIT");
            }
            if suffix == "csv" {
                sheet.typed_rows = sheet
                    .rows
                    .into_iter()
                    .map(|(r, values)| (r, values.into_iter().map(Value::String).collect()))
                    .collect();
            }
            for row in sheet.typed_rows.values_mut() {
                row.resize(sheet.columns, Value::Null);
            }
            if suffix == "csv" {
                if let Some((&header, headers)) = sheet.typed_rows.first_key_value() {
                    let headers = headers
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            v.as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("col_{}", i + 1))
                        })
                        .collect::<Vec<_>>();
                    let rows = sheet
                        .typed_rows
                        .range(header + 1..)
                        .map(|(_, row)| row.clone())
                        .collect::<Vec<_>>();
                    if !rows.is_empty() {
                        tables.push(json!({"sheet_name":"CSV","headers":headers,"rows":rows,"header_row":header+1,"row_numbers":sheet.typed_rows.range(header+1..).map(|(r,_)|r+1).collect::<Vec<_>>()}));
                    }
                }
                continue;
            }
            if let Some(mut table) =
                super::super::workbook_index::to_table(&sheet.name, sheet.typed_rows)
            {
                table["missing_formula_cache"] = json!(sheet.missing_formula_cache);
                tables.push(table);
            }
        }
        return Ok(
            json!({"text":"","sheets":tables,"extractor":"les.spreadsheet-tables.v1","formula_values":"saved_only"}),
        );
    }
    let mut parts = Vec::new();
    let mut metadata = Vec::new();
    let mut bytes = 0;
    for sheet in sheets {
        let (text, meta) = render(&sheet, request, limit, suffix == "csv")?;
        bytes += text.len();
        if bytes > MAX_TEXT {
            return Err(LIMIT);
        }
        if !text.is_empty() {
            parts.push(text);
        }
        metadata.push(meta);
    }
    Ok(
        json!({"text":parts.join("\n\n"),"sheets":metadata,"extractor":"les.spreadsheet-navigation.v1","formula_values":if suffix=="csv" {"not_applicable"} else {"saved_only"}}),
    )
}
