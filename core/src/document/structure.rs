//! Lossless reading units. Embedding budgets must never redefine table rows.
use serde_json::{json, Value};

pub(crate) fn units(text: &str, budget: usize) -> Result<Vec<Value>, &'static str> {
    if text.len() > 24 * 1024 * 1024 || budget == 0 {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    let mut lines: Vec<_> = text.split_inclusive('\n').collect();
    let mut out = Vec::new();
    let mut headings: Vec<(usize, String)> = Vec::new();
    let (mut i, mut offset, mut table_id) = (0, 0, 0);
    while i < lines.len() {
        let start = offset;
        let line = lines[i].trim();
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
            while headings.last().is_some_and(|h| h.0 >= level) {
                headings.pop();
            }
            headings.push((level, line.to_owned()));
        }
        let table = line.starts_with('|') && line.ends_with('|');
        let mut header = String::new();
        if table {
            table_id += 1;
            // Repeat only an explicit Markdown header, never guess the first
            // numeric row to be a label. The original header remains in body.
            if lines.get(i + 1).is_some_and(|l| separator(l)) {
                header = format!("{}{}", lines[i], lines[i + 1]);
                // Some exporters emit an empty Markdown header above the real
                // column labels. Preserve the next original row as opening
                // context; do not invent labels or drop the empty source row.
                if !lines[i].chars().any(char::is_alphanumeric) {
                    if let Some(row) = lines.get(i + 2).filter(|r| r.trim().starts_with('|')) {
                        header.push_str(row);
                    }
                }
            }
        }
        let mut row = 0;
        loop {
            let begin = offset;
            let first_row = row;
            let mut body = String::new();
            while i < lines.len() {
                let current = lines[i];
                let is_table = current.trim().starts_with('|') && current.trim().ends_with('|');
                if is_table != table {
                    break;
                }
                if !body.is_empty()
                    && (body.chars().count() + current.chars().count() > budget
                        || (!table && current.trim_start().starts_with('#')))
                {
                    break;
                }
                // A table row is atomic. Oversized rows are explicitly marked;
                // callers must report overflow rather than silently sever cells.
                if !table && current.chars().count() > budget {
                    if !body.is_empty() {
                        break;
                    }
                    let mut consumed = 0;
                    for word in current.split_inclusive(char::is_whitespace) {
                        if !body.is_empty() && body.chars().count() + word.chars().count() > budget
                        {
                            break;
                        }
                        body.push_str(word);
                        consumed += word.len();
                    }
                    if consumed < current.len() {
                        offset += consumed;
                        lines[i] = &current[consumed..];
                    } else {
                        offset += consumed;
                        i += 1;
                    }
                    break;
                }
                body.push_str(current);
                offset += current.len();
                i += 1;
                row += 1;
                if !table && (current.trim().is_empty() || level > 0) {
                    break;
                }
            }
            if !body.is_empty() {
                emit(
                    &mut out, &headings, &header, table, table_id, first_row, row, begin, &body,
                    budget,
                );
            }
            if i == lines.len()
                || (lines[i].trim().starts_with('|') && lines[i].trim().ends_with('|')) != table
            {
                break;
            }
            if !table {
                break;
            }
        }
        if offset == start {
            return Err("DOCUMENT_STRUCTURE_INVALID");
        }
        if out.len() > 50000 {
            return Err("INDEX_FILE_CHUNK_LIMIT");
        }
    }
    Ok(out)
}
fn separator(line: &str) -> bool {
    let line = line.trim().trim_matches('|');
    line.contains('-')
        && line
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t' | '\r'))
}
#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut Vec<Value>,
    headings: &[(usize, String)],
    header: &str,
    table: bool,
    table_id: usize,
    first: usize,
    end: usize,
    start: usize,
    body: &str,
    budget: usize,
) {
    let heading = headings
        .iter()
        .map(|h| h.1.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let prefix = format!(
        "{}{}",
        if !heading.is_empty() && !body.starts_with(&heading) {
            format!("{heading}\n\n")
        } else {
            String::new()
        },
        if table && first > 0 { header } else { "" }
    );
    let rendered = format!("{prefix}{body}");
    out.push(json!({"ordinal":out.len(),"kind":if table{"table_rows"}else{"paragraph"},
        "heading":heading,"table_id":if table{json!(table_id)}else{Value::Null},
        "row_start":if table{json!(first)}else{Value::Null},"row_end":if table{json!(end)}else{Value::Null},
        "start_byte":start,"end_byte":start+body.len(),"context_prefix":prefix,"body":body,
        "oversized":rendered.chars().count()>budget,"text":rendered}));
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    let text = request["text"]
        .as_str()
        .ok_or("DOCUMENT_STRUCTURE_INVALID")?;
    let budget = request["max_chars"]
        .as_u64()
        .unwrap_or(4000)
        .clamp(256, 12000) as usize;
    let versioned = json!(["les.document-reading.v1", budget, text]).to_string();
    Ok(
        json!({"schema":"les.document-reading.v1","view_sha256":crate::sha256_hex(versioned.as_bytes()),
        "units":units(text,budget)?}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_cells_are_atomic_and_missing_exporter_headers_are_not_invented() {
        let row = format!("|{}|17.25|\n", "Длинная ячейка ".repeat(100));
        let text = format!(
            "# Состав\n| | |\n|---|---|\n|Обозначение|Количество|\n{}{}",
            row,
            "|Кабель|2|\n".repeat(100)
        );
        let blocks = units(&text, 256).unwrap();
        assert_eq!(
            blocks
                .iter()
                .map(|u| u["body"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        let large = blocks
            .iter()
            .find(|u| u["body"].as_str().unwrap().contains("Длинная ячейка"))
            .unwrap();
        assert!(large["body"].as_str().unwrap().contains(&row));
        assert_eq!(large["oversized"], true);
        let last = blocks.last().unwrap();
        assert!(last["context_prefix"]
            .as_str()
            .unwrap()
            .contains("|Обозначение|Количество|"));
        let a = execute(&json!({"text":text,"max_chars":256})).unwrap();
        let b = execute(&json!({"text":text,"max_chars":1024})).unwrap();
        assert_ne!(a["view_sha256"], b["view_sha256"]);
    }
    #[test]
    fn body_ranges_cover_unicode_tables_and_long_paragraphs_exactly_once() {
        let text = format!(
            "# Раздел 🌲\n\n{}\n\n|Позиция|кВт|\n|---|---|\n{}\n## Итог\nСохранён.",
            "Абзац без разрыва слова. ".repeat(100),
            (0..80)
                .map(|i| format!("|Насос {i}|17.{i:02}|\n"))
                .collect::<String>()
        );
        let units = units(&text, 256).unwrap();
        assert_eq!(
            units
                .iter()
                .map(|u| u["body"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        let mut end = 0;
        for u in &units {
            assert_eq!(u["start_byte"], end);
            end = u["end_byte"].as_u64().unwrap();
            if u["kind"] == "table_rows" && u["row_start"].as_u64().unwrap() > 0 {
                assert!(u["text"].as_str().unwrap().contains("|Позиция|кВт|"));
            }
        }
        assert_eq!(end, text.len() as u64);
    }
}
