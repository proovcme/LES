//! A bounded, typed Docling projection shared by indexing and original reading.
//! No Markdown parser, model or I/O. Literal operators never become formatting.
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "DOCUMENT_STRUCTURE_INVALID";
pub(crate) const SCHEMA: &str = "les.document-structure.v1";

/// Inspect authoritative text leaves only, not JSON keys or presentation markup.
pub(crate) fn has_text(value: &Value, slot_prefix: Option<&str>) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|v| has_text(v, slot_prefix)),
        Value::Object(fields) => fields.iter().any(|(key, v)| {
            if key != "text" {
                return (v.is_object() || v.is_array()) && has_text(v, slot_prefix);
            }
            let Some(mut text) = v.as_str() else {
                return false;
            };
            if let Some(prefix) = slot_prefix {
                while let Some(at) = text.find(prefix) {
                    if text[..at].chars().any(char::is_alphanumeric) {
                        return true;
                    }
                    let Some(end) = text[at..].find("END") else {
                        return false;
                    };
                    text = &text[at + end + 3..];
                }
                text.chars().any(char::is_alphanumeric)
            } else {
                !text.trim().is_empty()
            }
        }),
        _ => false,
    }
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn plain(block: &Value, depth: usize) -> Result<String> {
    if depth > 128 {
        return Err(INVALID);
    }
    if let Some(text) = block["text"].as_str() {
        return Ok(text.to_owned());
    }
    if block["runs"].is_array() {
        return list(block, "runs")
            .iter()
            .map(|b| plain(b, depth + 1))
            .collect::<Result<Vec<_>>>()
            .map(|parts| {
                parts.join(if block["inline_whitespace"] == true {
                    ""
                } else {
                    " "
                })
            });
    }
    if block["kind"] == "table" {
        return grid(block, depth + 1).map(|rows| {
            rows.iter()
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("\n")
        });
    }
    Err(INVALID)
}
fn grid(block: &Value, depth: usize) -> Result<Vec<Vec<String>>> {
    let rows = block["rows"]
        .as_u64()
        .filter(|n| *n <= 100000)
        .ok_or(INVALID)? as usize;
    let cols = block["columns"]
        .as_u64()
        .filter(|n| *n <= 10000)
        .ok_or(INVALID)? as usize;
    if rows.checked_mul(cols).is_none_or(|n| n > 1_000_000) {
        return Err(INVALID);
    }
    let mut grid = vec![vec![String::new(); cols]; rows];
    let mut occupied = vec![vec![false; cols]; rows];
    let mut text_bytes = 0usize;
    for c in list(block, "cells") {
        let r = c["row"].as_u64().ok_or(INVALID)? as usize;
        let col = c["column"].as_u64().ok_or(INVALID)? as usize;
        let nr = c["row_span"].as_u64().filter(|n| *n > 0).ok_or(INVALID)? as usize;
        let nc = c["column_span"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or(INVALID)? as usize;
        if r.checked_add(nr).is_none_or(|n| n > rows)
            || col.checked_add(nc).is_none_or(|n| n > cols)
        {
            return Err(INVALID);
        }
        let text = list(c, "content")
            .iter()
            .map(|b| plain(b, depth + 1))
            .collect::<Result<Vec<_>>>()?
            .join("\n");
        text_bytes = text
            .len()
            .checked_mul(nr)
            .and_then(|n| n.checked_mul(nc))
            .and_then(|n| n.checked_add(text_bytes))
            .filter(|n| *n <= 24 * 1024 * 1024)
            .ok_or("DOCUMENT_TEXT_LIMIT")?;
        for row in r..r + nr {
            for column in col..col + nc {
                if occupied[row][column] {
                    return Err(INVALID);
                }
                occupied[row][column] = true;
                grid[row][column] = text.clone();
            }
        }
    }
    Ok(grid)
}
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '|' => out.push_str("&#124;"),
            '\\' | '*' | '_' | '`' | '[' | ']' | '#' | '~' => {
                out.push('\\');
                out.push(c);
            }
            '\r' => {}
            '\n' => out.push_str("<br>"),
            _ => out.push(c),
        }
    }
    out
}
fn row_text(row: &[String]) -> String {
    format!(
        "|{}|\n",
        row.iter().map(|s| escape(s)).collect::<Vec<_>>().join("|")
    )
}
fn block_text(block: &Value) -> Result<String> {
    if block["kind"] == "table" {
        let rows = grid(block, 0)?;
        let mut out = String::new();
        for (i, row) in rows.iter().enumerate() {
            out.push_str(&row_text(row));
            if i == 0 {
                out.push_str(&format!("|{}|\n", vec!["---"; row.len()].join("|")));
            }
        }
        return Ok(out);
    }
    let text = escape(&plain(block, 0)?);
    Ok(if block["kind"] == "heading" {
        format!(
            "{} {text}\n",
            "#".repeat(block["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize)
        )
    } else {
        format!("{text}\n")
    })
}
pub(crate) fn render(structure: &Value) -> Result<String> {
    if structure["schema"] != SCHEMA || list(structure, "blocks").len() > 100000 {
        return Err(INVALID);
    }
    let mut out = String::new();
    for block in list(structure, "blocks") {
        out.push_str(&block_text(block)?);
        out.push('\n');
        if out.len() > 24 * 1024 * 1024 {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
    }
    Ok(out)
}
pub(crate) fn units(structure: &Value, budget: usize) -> Result<Vec<Value>> {
    if structure["schema"] != SCHEMA || budget == 0 {
        return Err(INVALID);
    }
    let mut out = Vec::new();
    let mut offset = 0;
    let mut headings: Vec<(u64, String)> = Vec::new();
    for block in list(structure, "blocks") {
        let body = format!("{}\n", block_text(block)?);
        if block["kind"] == "heading" {
            let level = block["level"].as_u64().unwrap_or(1);
            while headings.last().is_some_and(|(l, _)| *l >= level) {
                headings.pop();
            }
            headings.push((level, body.trim().to_owned()));
        }
        let heading = headings
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let prefix = if block["kind"] == "heading" || heading.is_empty() {
            String::new()
        } else {
            format!("{heading}\n\n")
        };
        if block["kind"] == "table" {
            let rows = grid(block, 0)?;
            let header = rows
                .first()
                .map(|r| format!("{}|{}|\n", row_text(r), vec!["---"; r.len()].join("|")))
                .unwrap_or_default();
            let mut start = 0;
            let mut bytes = 0;
            while start < rows.len() {
                let mut end = start;
                let mut text = String::new();
                while end < rows.len() {
                    let mut line = row_text(&rows[end]);
                    if end == 0 {
                        line.push_str(&format!("|{}|\n", vec!["---"; rows[end].len()].join("|")));
                    }
                    if end + 1 == rows.len() {
                        line.push('\n');
                    }
                    if end > start
                        && prefix.chars().count()
                            + text.chars().count()
                            + line.chars().count()
                            + if start > 0 { header.chars().count() } else { 0 }
                            > budget
                    {
                        break;
                    }
                    text.push_str(&line);
                    end += 1;
                }
                let context = format!("{prefix}{}", if start > 0 { &header } else { "" });
                let data = json!({"schema":SCHEMA,"kind":"table","rows":rows[start..end],"header":rows.first(),"source_block_id":block["id"],"source_row_start":start,"source_row_end":end});
                emit(
                    &mut out,
                    block,
                    &heading,
                    &context,
                    &text,
                    &header,
                    offset + bytes,
                    budget,
                    Some((start, end)),
                    data,
                );
                bytes += text.len();
                start = end;
            }
        } else {
            // Cut literal text, never a Markdown escape/entity. Each fragment
            // retains only its own raw paragraph for model row selection.
            let raw = plain(block, 0)?;
            let mut parts = Vec::new();
            let mut current = String::new();
            let mut cost = 0;
            for word in raw.split_inclusive(char::is_whitespace) {
                let n = escape(word).chars().count();
                if !current.is_empty() && cost + n > budget {
                    parts.push(std::mem::take(&mut current));
                    cost = 0;
                }
                for ch in word.chars() {
                    let n = escape(&ch.to_string()).chars().count();
                    if !current.is_empty() && cost + n > budget {
                        parts.push(std::mem::take(&mut current));
                        cost = 0;
                    }
                    current.push(ch);
                    cost += n;
                }
            }
            if !current.is_empty() || parts.is_empty() {
                parts.push(current);
            }
            let mut start = 0;
            for (i, raw) in parts.iter().enumerate() {
                let mut text = escape(raw);
                if i == 0 && block["kind"] == "heading" {
                    text = format!(
                        "{} {text}",
                        "#".repeat(block["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize)
                    );
                }
                if i + 1 == parts.len() {
                    text.push_str("\n\n");
                }
                let data = json!({"schema":SCHEMA,"kind":"paragraph","source_block_id":block["id"],"text":raw});
                emit(
                    &mut out,
                    block,
                    &heading,
                    &prefix,
                    &text,
                    "",
                    offset + start,
                    budget,
                    None,
                    data,
                );
                start += text.len();
            }
        }
        offset += body.len();
        if out.len() > 50000 || offset > 24 * 1024 * 1024 {
            return Err("DOCUMENT_STRUCTURE_LIMIT");
        }
    }
    Ok(out)
}
#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut Vec<Value>,
    block: &Value,
    heading: &str,
    prefix: &str,
    body: &str,
    header: &str,
    start: usize,
    budget: usize,
    rows: Option<(usize, usize)>,
    data: Value,
) {
    let text = format!("{prefix}{body}");
    out.push(json!({"ordinal":out.len(),"kind":if rows.is_some(){"table_rows"}else if block["kind"]=="heading"{"heading"}else{"paragraph"},"heading":heading,
        "source_block_id":block["id"],"source_parent_id":block["parent"],"table_id":if rows.is_some(){block["id"].clone()}else{Value::Null},
        "row_start":rows.map(|r|r.0),"row_end":rows.map(|r|r.1),"start_byte":start,"end_byte":start+body.len(),
        "context_prefix":prefix,"body":body,"table_header":header,"oversized":text.chars().count()>budget,"text":text,"canonical":data}));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cell(row: usize, column: usize, text: &str) -> Value {
        json!({"row":row,"column":column,"row_span":1,"column_span":1,"content":[{"kind":"paragraph","text":text}]})
    }
    #[test]
    fn literal_runs_spans_and_rows_survive_without_markdown_inference() {
        let mut merged = cell(0, 0, "Position | quantity");
        merged["column_span"] = json!(2);
        let structure = json!({"schema":SCHEMA,"blocks":[
            {"id":"heading","kind":"heading","text":"Equipment","level":2},
            {"id":"table","parent":"heading","kind":"table","rows":3,"columns":2,"cells":[merged,
                cell(1,0,"125*480 / [DN_100]"),cell(1,1,"17.25"),
                cell(2,0,"Valve\ncontinued"),cell(2,1,"2")]},
            {"id":"p","kind":"paragraph","inline_whitespace":true,"runs":[
                {"text":"Для сечения 70/120 мм","formatting":{"italic":true}},
                {"text":"2","formatting":{"italic":true,"script":"Super"}}]}]});
        let text = render(&structure).unwrap();
        assert!(text.contains("125\\*480 / \\[DN\\_100\\]"));
        assert!(text.contains("Для сечения 70/120 мм2"));
        let units = units(&structure, 100).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|u| u["body"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        let table = units.iter().find(|u| u["kind"] == "table_rows").unwrap();
        assert_eq!(table["source_parent_id"], "heading");
        let rows = units
            .iter()
            .filter(|u| u["kind"] == "table_rows")
            .flat_map(|u| list(&u["canonical"], "rows").iter())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1][0], "125*480 / [DN_100]");
        assert_eq!(rows[1][1], "17.25");
        assert_eq!(rows[2][0], "Valve\ncontinued");
    }
    #[test]
    fn long_literal_paragraphs_cover_ranges_once_without_cutting_escapes() {
        let raw = "Кабель 125*480 <2> [DN_100] ".repeat(100);
        let s = json!({"schema":SCHEMA,"blocks":[{"id":"p","kind":"paragraph","text":raw}]});
        let text = render(&s).unwrap();
        let units = units(&s, 100).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|u| u["canonical"]["text"].as_str().unwrap())
                .collect::<String>(),
            raw
        );
        assert_eq!(
            units
                .iter()
                .map(|u| u["body"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        let mut end = 0;
        for u in units {
            assert_eq!(u["start_byte"], end);
            end = u["end_byte"].as_u64().unwrap();
        }
        assert_eq!(end as usize, text.len());
    }
    #[test]
    fn invalid_or_overlapping_spans_fail_instead_of_inventing_cells() {
        let s = json!({"schema":SCHEMA,"blocks":[{"kind":"table","rows":1,"columns":1,"cells":[cell(0,0,"17.25"),cell(0,0,"117.25")]}]});
        assert_eq!(render(&s).unwrap_err(), INVALID);
    }
}
