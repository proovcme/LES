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

fn header_score(values: &[Value]) -> usize {
    values
        .iter()
        .map(|value| {
            let text = as_text(value);
            let text = trim(&text);
            if text.is_empty() {
                0
            } else if value.is_string() && text.chars().count() > 1 {
                3
            } else {
                1
            }
        })
        .sum()
}

fn to_table(name: &str, rows: BTreeMap<u32, Vec<Value>>) -> Option<Value> {
    let mut best_score = 0;
    let mut header = 0;
    for (&row, values) in rows.range(..20) {
        let score = header_score(values);
        if score > best_score {
            header = row;
            best_score = score;
        }
    }
    if best_score == 0 {
        return None;
    }
    let headers: Vec<String> = rows[&header]
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let text = as_text(value);
            let text = trim(&text);
            if text.is_empty() {
                format!("col_{}", index + 1)
            } else {
                text.to_owned()
            }
        })
        .collect();
    let (row_numbers, data): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .filter_map(|(row, values)| {
            if row <= header || header_score(&values) == 0 {
                return None;
            }
            Some((row + 1, values))
        })
        .unzip();
    if data.is_empty() {
        return None;
    }
    Some(json!({"sheet_name":name,"headers":headers,"rows":data,"header_row":header+1,"row_numbers":row_numbers}))
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
        } else if last_row > 0 {
            if let Some(table) = to_table(&sheet.name, rows) {
                let bytes = serde_json::to_vec(&table).map_err(|_| COMPAT)?.len();
                estimated_bytes += bytes;
                if estimated_bytes > MAX_OUTPUT {
                    return Err(COMPAT);
                }
                tables.push(table);
            }
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
