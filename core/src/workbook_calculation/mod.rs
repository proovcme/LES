//! Built-in workbook calculations. External skill code is never evaluated here.
pub(crate) mod decimal;
mod ranges;
use crate::workspace::{Failure, Result};
use decimal::Decimal;
use ranges::{coordinate, Region};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
};

fn invalid() -> Failure {
    Failure::new(
        "WORKBOOK_SNAPSHOT_INVALID",
        "Не удалось проверить структуру книги для расчёта",
    )
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value[key].as_array().ok_or_else(invalid)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(invalid)
}
fn sheets(document: &Value) -> Result<HashMap<&str, &Value>> {
    let mut sheets = HashMap::new();
    for sheet in array(document, "sheets")? {
        if sheets.insert(text(sheet, "name")?, sheet).is_some() {
            return Err(invalid());
        }
    }
    if sheets.len() > 10000 {
        return Err(invalid());
    }
    Ok(sheets)
}
fn describe(document: &Value) -> Result<Value> {
    sheets(document)?;
    let mut last = HashMap::<&str, u64>::new();
    for row in array(document, "rows")? {
        let name = text(row, "sheet")?;
        let number = row["row"]
            .as_u64()
            .filter(|n| (1..=20000).contains(n))
            .ok_or_else(invalid)?;
        last.entry(name)
            .and_modify(|v| *v = (*v).max(number))
            .or_insert(number);
    }
    let mut out = Vec::new();
    for sheet in array(document, "sheets")? {
        let name = text(sheet, "name")?;
        let columns = sheet["columns"]
            .as_u64()
            .filter(|n| (1..=512).contains(n))
            .ok_or_else(invalid)? as u32;
        let hidden = sheet["hidden"].as_bool().ok_or_else(invalid)?;
        out.push(json!({"name":name,"last_row":last.get(name).copied().unwrap_or(0),"last_column":ranges::column(columns),"hidden":hidden}));
    }
    Ok(json!({"sheets":out}))
}
fn calculate(document: &Value, request: &Value) -> Result<Value> {
    let requested = request["ranges"]
        .as_array()
        .filter(|v| (1..=8).contains(&v.len()))
        .ok_or_else(|| Failure::new("WORKBOOK_RANGE_COUNT", "Укажите от 1 до 8 диапазонов"))?;
    let sheets = sheets(document)?;
    let mut selected: Vec<(&str, Region)> = Vec::new();
    let mut cells = 0;
    for item in requested {
        let name = text(item, "sheet")?;
        let address = text(item, "range")?;
        let sheet = sheets.get(name).ok_or_else(ranges::invalid)?;
        let region = Region::parse(address)?;
        if selected
            .iter()
            .any(|(prior_name, prior)| *prior_name == name && prior.intersects(&region))
        {
            return Err(Failure::new(
                "WORKBOOK_RANGES_OVERLAP",
                "Диапазоны на одном листе пересекаются",
            ));
        }
        for merged in array(sheet, "merged_ranges")? {
            if Region::parse(merged.as_str().ok_or_else(invalid)?)?.intersects(&region) {
                return Err(Failure::new(
                    "WORKBOOK_RANGE_MERGED",
                    format!("{name}!{} содержит объединённые ячейки", region.display()),
                ));
            }
        }
        cells += region.size();
        if cells > 300000 {
            return Err(Failure::new(
                "WORKBOOK_RANGE_LIMIT",
                "Суммарный диапазон превышает 300000 ячеек",
            ));
        }
        selected.push((name, region));
    }
    let rows = array(document, "rows")?;
    let count = rows.iter().try_fold(0_usize, |sum, row| {
        Ok::<_, Failure>(sum + array(row, "cells")?.len())
    })?;
    if rows.len() > 300000 || count > 300000 {
        return Err(invalid());
    }
    let mut subtotals = Vec::new();
    let mut total = Decimal::default();
    for (name, region) in selected {
        let mut subtotal = Decimal::default();
        let (mut numeric, mut cached) = (0_u64, 0_u64);
        let mut seen = HashSet::new();
        for row in rows {
            if text(row, "sheet")? != name {
                continue;
            }
            let number = row["row"]
                .as_u64()
                .filter(|v| (1..=20000).contains(v))
                .ok_or_else(invalid)? as u32;
            if !(region.first_row..=region.last_row).contains(&number) {
                continue;
            }
            for cell in array(row, "cells")? {
                let address = text(cell, "coordinate")?;
                let (physical_row, column) = coordinate(address)?;
                if physical_row != number {
                    return Err(invalid());
                }
                if !region.contains(number, column) {
                    continue;
                }
                if !seen.insert((number, column)) {
                    return Err(invalid());
                }
                let location = format!("{name}!{address}");
                let value = if cell["kind"] == "formula" {
                    let value = &cell["cached"];
                    if cell["needs_recalculation"] == true
                        || !value.is_object()
                        || value["kind"] == "error"
                    {
                        return Err(Failure::new(
                            "WORKBOOK_FORMULA_CACHE_MISSING",
                            format!("У {location} нет сохранённого результата формулы"),
                        ));
                    }
                    cached += 1;
                    value
                } else {
                    cell
                };
                if value["kind"] != "number" {
                    return Err(Failure::new(
                        "WORKBOOK_CELL_NOT_NUMBER",
                        format!("{location} содержит значение, которое нельзя сложить как число"),
                    ));
                }
                subtotal.add(&Decimal::parse(text(value, "value")?)?)?;
                numeric += 1;
            }
        }
        subtotals.push(json!({"sheet":name,"range":region.display(),"first_row":region.first_row,
            "sum":subtotal.display(),"numeric_cells":numeric,"blank_cells":region.size()-numeric,"cached_formulas":cached}));
        total.add(&subtotal)?;
    }
    let sha = text(document, "sha256")?;
    if sha.len() != 64 || !sha.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(json!({"total":total.display(),"ranges":subtotals,"source_sha256":sha}))
}
pub fn execute(request: &Value, document: &Value) -> Value {
    let operation = || -> Result<Value> {
        if request["schema"] != "les.workbook-calculation.v1" {
            return Err(invalid());
        }
        match request["action"].as_str() {
            Some("sum") => calculate(document, request),
            Some("describe") => describe(document),
            _ => Err(invalid()),
        }
    };
    match operation() {
        Ok(value) => json!({"ok":true,"value":value,"owner":"rust.workbook-calculation.v1"}),
        Err(error) => json!({"ok":false,"error":{"code":error.code,"message":error.detail}}),
    }
}
pub(crate) fn run(
    request: &Value,
    bridge: impl Fn(u8, &Value) -> std::result::Result<Value, &'static str>,
) -> std::result::Result<Value, &'static str> {
    let mut input = String::new();
    let mut offset = 0;
    loop {
        let reply = bridge(0, &json!({"offset":offset,"limit":262144}))?;
        let text = reply["text"].as_str().ok_or("WORKBOOK_SNAPSHOT_INVALID")?;
        let count = text.chars().count();
        if count > 262144 || (reply["complete"] != true && count != 262144) {
            return Err("WORKBOOK_SNAPSHOT_INVALID");
        }
        if input.len() + text.len() > 256 * 1024 * 1024 {
            return Err("WORKBOOK_SNAPSHOT_LIMIT");
        }
        input.push_str(text);
        offset += count;
        if reply["complete"] == true {
            break;
        }
    }
    let document = serde_json::from_str(&input).map_err(|_| "WORKBOOK_SNAPSHOT_INVALID")?;
    Ok(execute(request, &document))
}
/// A local operator's native CLI: source path + explicit ranges, no interpreter.
pub fn serve_once() -> std::result::Result<(), &'static str> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(262145)
        .read_to_end(&mut bytes)
        .map_err(|_| "WORKBOOK_REQUEST_INVALID")?;
    if bytes.len() > 262144 {
        return Err("WORKBOOK_REQUEST_LIMIT");
    }
    let request: Value = serde_json::from_slice(&bytes).map_err(|_| "WORKBOOK_REQUEST_INVALID")?;
    let path = std::path::Path::new(request["path"].as_str().ok_or("WORKBOOK_REQUEST_INVALID")?);
    if !path.is_absolute() {
        return Err("WORKBOOK_REQUEST_INVALID");
    }
    let reply = match crate::document::workbook::snapshot(&json!({"path":path})) {
        Ok(document) => execute(&request, &document),
        Err(code) => {
            json!({"ok":false,"error":{"code":code,"message":"Не удалось прочитать книгу нативным обработчиком"}})
        }
    };
    writeln!(std::io::stdout(), "{reply}").map_err(|_| "WORKBOOK_RESPONSE_FAILED")
}
