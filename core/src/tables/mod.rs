//! Table values are source data. No domain workflow or model calls live here.
pub mod artifacts;
mod normalize;
mod prepare;
mod storage;
use serde_json::{json, Value};
use std::sync::LazyLock;
pub(super) type Result<T> = std::result::Result<T, &'static str>;
pub(super) const INVALID: &str = "TABLE_REQUEST_INVALID";
pub(super) const LIMIT: &str = "TABLE_LIMIT";
pub(super) const MAX_ROWS: usize = 1_000_000;
pub(super) static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../config/table-row-schema.json"))
        .expect("versioned table schema")
});

pub fn execute(request: &Value) -> Result<Value> {
    match request["action"].as_str() {
        Some("normalize") => normalize::rows(request),
        Some("prepare") => prepare::prepare(request),
        Some("chunks") => normalize::chunks(request),
        Some("write") => storage::write(request),
        Some("read") => storage::read(request),
        Some("schema") => Ok(SCHEMA.clone()),
        Some("references") => artifacts::refs(request),
        Some("sum_rows") => sum_rows(request),
        _ => Err(INVALID),
    }
}

fn sum_rows(request: &Value) -> Result<Value> {
    use crate::workbook_calculation::decimal::Decimal;
    let key = field(request, "field");
    if !SCHEMA["numeric"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == key)
    {
        return Err(INVALID);
    }
    let rows = storage::rows(request)?;
    let mut total = Decimal::default();
    let mut count = 0;
    for row in &rows {
        let value = &row[key];
        if value.is_null() || value.is_boolean() {
            continue;
        }
        let text = string(value);
        let Ok(value) = Decimal::parse(text.trim()) else {
            continue;
        };
        total.add(&value).map_err(|_| "TABLE_NUMBER_PRECISION")?;
        count += 1;
    }
    let text = total.display();
    let mut number = serde_json::from_str::<Value>(&text).map_err(|_| "TABLE_NUMBER_PRECISION")?;
    if number.as_f64().is_none_or(|v| !v.is_finite()) {
        number = json!(text);
    }
    Ok(
        json!({"total":number,"exact_total":text,"count":count,"missing":rows.len()-count,
        "complete":count==rows.len(),"field":key,"owner":"rust.tables.sum"}),
    )
}

pub(super) fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
pub(super) fn string(value: &Value) -> String {
    match value {
        Value::String(v) => v.clone(),
        Value::Null => "None".into(),
        Value::Bool(v) => if *v { "True" } else { "False" }.into(),
        _ => value.to_string(),
    }
}
pub(super) fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(super) fn output(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value).map_err(|_| INVALID)?.len() > 30 * 1024 * 1024 {
        Err(LIMIT)
    } else {
        Ok(value)
    }
}

pub fn serve() -> Result<()> {
    use std::io::{BufRead, Read, Write};
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut output = std::io::stdout().lock();
    loop {
        let mut frame = Vec::new();
        if input
            .by_ref()
            .take(32 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut frame)
            .map_err(|_| "TABLE_INPUT_FAILED")?
            == 0
        {
            return Ok(());
        }
        if frame.len() > 32 * 1024 * 1024 || !frame.ends_with(b"\n") {
            return Err(LIMIT);
        }
        let envelope: Value = serde_json::from_slice(&frame).map_err(|_| INVALID)?;
        let result = match execute(&envelope["input"]) {
            Ok(v) => json!({"status":"completed","value":v}),
            Err(code) => {
                eprintln!("{}", json!({"event":"table_operation_failed","code":code}));
                json!({"status":"failed","code":code})
            }
        };
        serde_json::to_writer(
            &mut output,
            &json!({"request_id":envelope["request_id"],"result":result}),
        )
        .map_err(|_| "TABLE_OUTPUT_FAILED")?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|_| "TABLE_OUTPUT_FAILED")?;
    }
}
