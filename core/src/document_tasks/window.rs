//! Addressed reads do not change review progress. Cursors count Unicode characters.
use super::{encode, number, text};
use crate::workspace::{Failure, Result};
use serde_json::{json, Value};
fn column(mut n: i64) -> String {
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push((b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    chars.iter().rev().collect()
}
pub(super) fn address(s: &str) -> Option<(i64, i64)> {
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let c = letters.bytes().try_fold(0i64, |n, b| {
        n.checked_mul(26)?.checked_add((b - b'A' + 1) as i64)
    })?;
    let r = digits.parse::<i64>().ok()?;
    (r > 0 && c > 0).then_some((r, c))
}
pub(super) fn execute(doc: &Value, action: &str, args: &Value) -> Result<Value> {
    if !["read_window", "read_cell"].contains(&action) {
        return Err(Failure::invalid("Неизвестное действие задания"));
    }
    let sheet = text(args, "sheet")?;
    let metadata = doc["sheets"]
        .as_array()
        .and_then(|v| v.iter().find(|s| s["name"] == sheet))
        .ok_or_else(|| {
            Failure::invalid("Неизвестный лист; используйте точное имя из списка sheets")
        })?;
    let rows: Vec<_> = doc["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["sheet"] == sheet)
        .collect();
    if action == "read_cell" {
        let coordinate = text(args, "coordinate")?;
        let field = args["field"].as_str().unwrap_or("value");
        let offset = number(args, "offset", 0)?;
        let count = number(args, "max_chars", 1000)?;
        if offset < 0
            || !(128..=3000).contains(&count)
            || !["value", "formula", "cached"].contains(&field)
        {
            return Err(Failure::invalid("Некорректная часть ячейки"));
        }
        let cell = rows
            .iter()
            .flat_map(|r| r["cells"].as_array().into_iter().flatten())
            .find(|c| c["coordinate"] == coordinate)
            .ok_or_else(|| {
                Failure::invalid(
                    "Нет сохранённого значения этой ячейки; проверьте окно и объединения",
                )
            })?;
        let value = cell.get(field).ok_or_else(|| {
            Failure::invalid(
                "В ячейке нет выбранного поля; для формулы используйте formula или cached",
            )
        })?;
        let literal = if let Some(s) = value.as_str() {
            s.to_owned()
        } else {
            encode(value)?
        };
        let chars: Vec<_> = literal.chars().collect();
        if offset as usize > chars.len() || (offset != 0 && offset as usize == chars.len()) {
            return Err(Failure::invalid("Смещение за пределами значения ячейки"));
        }
        let stop = chars.len().min(offset as usize + count as usize);
        return Ok(
            json!({"sheet":sheet,"coordinate":coordinate,"kind":cell["kind"],"field":field,"source_sha256":doc["sha256"],
            "offset":offset,"text":chars[offset as usize..stop].iter().collect::<String>(),"representation":if value.is_string(){"literal_text"}else{"json"},
            "next_offset":if stop<chars.len(){json!(stop)}else{Value::Null},"total_chars":chars.len(),"read_advances_review":false}),
        );
    }
    let row = number(args, "row_start", 1)?;
    let col = number(args, "column_start", 1)?;
    let height = number(args, "row_count", 4)?;
    let width = number(args, "column_count", 8)?;
    let budget = number(args, "max_bytes", 6000)?;
    if !(1..=20000).contains(&row)
        || !(1..=512).contains(&col)
        || !(1..=4).contains(&height)
        || !(1..=8).contains(&width)
        || !(512..=12000).contains(&budget)
    {
        return Err(Failure::invalid(
            "Нужно окно до 4 строк и 8 столбцов в пределах таблицы",
        ));
    }
    let last = doc["source_height"].as_i64().unwrap_or_else(|| {
        rows.iter()
            .filter_map(|r| r["row"].as_i64())
            .max()
            .unwrap_or(0)
    });
    let columns = metadata["columns"].as_i64().unwrap_or(0);
    if row > last || col > columns {
        return Err(Failure::invalid(
            "Окно за пределами непустой области таблицы",
        ));
    }
    let endrow = last.min(row + height - 1);
    let endcol = columns.min(col + width - 1);
    let allowance = 64.max((budget - 500) / ((endrow - row + 1) * (endcol - col + 1)));
    let merges: Vec<_> = metadata["merged_ranges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let (a, b) = v.as_str()?.split_once(':')?;
            Some((address(a)?, address(b)?))
        })
        .collect();
    let mut cells = Vec::new();
    for r in row..=endrow {
        for c in col..=endcol {
            let coordinate = format!("{}{r}", column(c));
            let mut cell = rows
                .iter()
                .filter(|v| v["row"] == r)
                .flat_map(|v| v["cells"].as_array().into_iter().flatten())
                .find(|v| v["coordinate"] == coordinate)
                .cloned()
                .unwrap_or_else(|| json!({"coordinate":coordinate,"kind":"blank_or_absent"}));
            if let Some(((ar, ac), _)) = merges
                .iter()
                .find(|((ar, ac), (br, bc))| r >= *ar && r <= *br && c >= *ac && c <= *bc)
            {
                let anchor = format!("{}{ar}", column(*ac));
                if anchor != coordinate {
                    cell = json!({"coordinate":coordinate,"kind":"merged_continuation","anchor":anchor});
                }
            }
            if encode(&cell)?.len() > allowance as usize {
                cell = json!({"coordinate":coordinate,"kind":cell["kind"],"deferred":true,"read":"read_cell"});
            }
            cells.push(cell);
        }
    }
    let cursor = if endcol < columns {
        json!({"row_start":row,"column_start":endcol+1})
    } else if endrow < last {
        json!({"row_start":endrow+1,"column_start":1})
    } else {
        Value::Null
    };
    let result = json!({"sheet":sheet,"source_sha256":doc["sha256"],"cells":cells,"bounds":[row,col,endrow,endcol],"next_cursor":cursor,
        "read_advances_review":false,"blank_policy":"Snapshot does not distinguish absent and explicit empty cells."});
    if encode(&result)?.len() > budget as usize {
        return Err(Failure::invalid(
            "Окно превышает бюджет; уменьшите row_count или column_count",
        ));
    }
    Ok(result)
}
