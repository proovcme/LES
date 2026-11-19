//! Excel's saved values for the existing document probe and Parquet index.
use super::workbook::{dimensions, verified, Result, COMPAT};
use calamine::{DataRef, Reader, SheetType, Xlsx};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Cursor};

const MAX_OUTPUT: usize = 28 * 1024 * 1024;

fn value(cell: &DataRef<'_>) -> Result<Value> {
    Ok(match cell {
        DataRef::Empty => Value::Null,
        DataRef::Int(value) => json!(value),
        DataRef::Float(value) if value.is_finite() && value.abs() < 9_007_199_254_740_992.0 => {
            if value.fract() == 0.0 {
                json!(*value as i64)
            } else {
                json!(value)
            }
        }
        DataRef::Bool(value) => json!(value),
        DataRef::Error(value) => json!(value.to_string()),
        DataRef::String(value) => json!(value),
        DataRef::SharedString(value) => json!(value),
        _ => return Err(COMPAT),
    })
}

fn as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

pub(super) fn to_table(name: &str, mut rows: BTreeMap<u32, Vec<Value>>) -> Option<Value> {
    rows.retain(|_, row| row.iter().any(|cell| !trim(&as_text(cell)).is_empty()));
    let width = rows.values().map(Vec::len).max()?;
    // Scan the first populated rows so blank space before a header does not
    // affect recognition. A generic text header can precede typed data.
    let header =
        rows.iter()
            .zip(rows.values().skip(1))
            .take(20)
            .find_map(|((&row, values), next)| {
                let labels: Vec<_> = values
                    .iter()
                    .map(|v| trim(&as_text(v)).to_owned())
                    .collect();
                let populated = values
                    .iter()
                    .zip(&labels)
                    .filter(|(_, label)| !label.is_empty());
                let text_count = populated
                    .clone()
                    .filter(|(value, _)| value.is_string())
                    .count();
                let typed_data = next
                    .iter()
                    .any(|value| value.is_number() || value.is_boolean());
                (crate::tables::has_column_labels(&labels)
                    || text_count >= 2 && text_count == populated.count() && typed_data)
                    .then_some(row)
            });
    let header_values = header.and_then(|row| rows.remove(&row));
    let headers: Vec<_> = (0..width)
        .map(|i| {
            header_values
                .as_ref()
                .and_then(|values| values.get(i))
                .map(as_text)
                .map(|text| trim(&text).to_owned())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| format!("col_{}", i + 1))
        })
        .collect();
    let (row_numbers, data): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .map(|(row, mut values)| {
            values.resize(width, Value::Null);
            (row + 1, values)
        })
        .unzip();
    Some(json!({"sheet_name":name,"headers":headers,"rows":data,
        "header_row":header.map(|row|row+1),"header_values":header_values,"row_numbers":row_numbers}))
}

fn extract(bytes: &[u8], probe: bool) -> Result<Value> {
    let mut book = Xlsx::new(Cursor::new(bytes)).map_err(|_| COMPAT)?;
    let sheets = book.sheets_metadata().to_vec();
    if sheets.iter().any(|sheet| sheet.typ != SheetType::WorkSheet) {
        return Err(COMPAT);
    }
    let mut tables = Vec::new();
    let mut samples = Vec::new();
    let mut row_total = 0;
    let mut column_max = 0;
    let mut estimated_bytes = 0;
    for sheet in sheets.iter().take(if probe { 3 } else { sheets.len() }) {
        let mut reader = book
            .worksheet_cells_reader(&sheet.name)
            .map_err(|_| COMPAT)?;
        let (last_row, last_col) = reader.dimensions().end;
        dimensions((last_row, last_col)).map_err(|_| COMPAT)?;
        let width = (last_col + 1) as usize;
        let mut rows = BTreeMap::new();
        let mut previous = None;
        while let Some(cell) = reader.next_cell().map_err(|_| COMPAT)? {
            let (row, col) = cell.get_position();
            let position = (row, col);
            if row > last_row || col > last_col || previous.is_some_and(|old| old >= position) {
                return Err(COMPAT);
            }
            previous = Some(position);
            if probe && row >= 5 {
                break;
            }
            let scalar = value(cell.get_value())?;
            if scalar.is_null() {
                continue;
            }
            // Account for large text before retaining it. The wire shape keeps
            // the header once and sends values in its column order.
            estimated_bytes += serde_json::to_vec(&scalar).map_err(|_| COMPAT)?.len() + 1;
            if estimated_bytes > MAX_OUTPUT {
                return Err(COMPAT);
            }
            rows.entry(row).or_insert_with(|| vec![Value::Null; width])[col as usize] = scalar;
        }
        if probe {
            row_total += last_row + 1;
            column_max = column_max.max(last_col + 1);
            for row in 0..(last_row + 1).min(5) {
                samples.push(rows.get(&row).map_or_else(
                    || " | ".repeat(width - 1),
                    |values| values.iter().map(as_text).collect::<Vec<_>>().join(" | "),
                ));
            }
        } else if let Some(table) = to_table(&sheet.name, rows) {
            let bytes = serde_json::to_vec(&table).map_err(|_| COMPAT)?.len();
            estimated_bytes += bytes;
            if estimated_bytes > MAX_OUTPUT {
                return Err(COMPAT);
            }
            tables.push(table);
        }
    }
    if probe {
        Ok(
            json!({"sheet_count":sheets.len(),"row_count_hint":row_total,"column_count_hint":column_max,
            "text_sample":samples.join("\n").chars().take(6000).collect::<String>()}),
        )
    } else {
        Ok(json!({"sheets":tables}))
    }
}

pub fn execute(request: &Value) -> Result<Value> {
    let probe = match request["action"].as_str() {
        Some("probe") => true,
        Some("read") => false,
        _ => return Err("WORKBOOK_INDEX_ACTION_INVALID"),
    };
    verified(request, |bytes, _| extract(bytes, probe))
}
