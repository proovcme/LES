//! A tokenizer limit may split prose, but never silently turn half a table row
//! into a complete row. Repeated headings/captions remain original source text.
use super::{length, prefix, Measure};
use crate::document::chunks::{offsets, trim};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, &'static str>;

struct Builder<'a> {
    budget: usize,
    overlap: usize,
    search: &'a str,
    measure: Option<&'a Measure<'a>>,
    parts: Vec<Value>,
}
impl Builder<'_> {
    fn cost(&self, context: &str, body: &str) -> Result<usize> {
        length(
            &format!("{}{context}{}", self.search, trim(body)),
            self.measure,
        )
    }
    fn emit(&mut self, context: &str, body: &str, partial: bool) -> Result<()> {
        if trim(body).is_empty() {
            return Ok(());
        }
        let text = format!("{context}{}", trim(body));
        let cost = self.cost("", &text)?;
        if cost > self.budget {
            return Err("DOCUMENT_EMBEDDING_BUDGET_EXCEEDED");
        }
        if self.parts.len() >= 50_000 {
            return Err("INDEX_FILE_CHUNK_LIMIT");
        }
        self.parts.push(json!({"text":text,"length":cost,"ord":self.parts.len(),"requires_source_read":partial}));
        Ok(())
    }
    fn prose(&mut self, context: &str, body: &str, partial: bool) -> Result<()> {
        if !context.is_empty() && self.cost(context, "")? >= self.budget {
            // Even a literal heading may exceed an embedding window. Keep its
            // search slices, but require the original for interpreting the body.
            self.prose("", context, true)?;
            return self.prose("", body, true);
        }
        if self.cost(context, body)? <= self.budget {
            return self.emit(context, body, partial);
        }
        let indices = offsets(body);
        let count = indices.len() - 1;
        let mut start = 0;
        while start < count {
            let measure = |s: &str| self.cost(context, s);
            let end = if measure(&body[indices[start]..])? <= self.budget {
                count
            } else {
                prefix(body, &indices, start, self.budget, Some(&measure))?
            };
            self.emit(context, &body[indices[start]..indices[end]], partial)?;
            if end == count {
                break;
            }
            let mut next = end;
            if !partial && self.overlap > 0 {
                let (mut low, mut high) = (start + 1, end);
                while low < high {
                    let mid = (low + high) / 2;
                    if length(&body[indices[mid]..indices[end]], self.measure)? <= self.overlap {
                        high = mid;
                    } else {
                        low = mid + 1;
                    }
                }
                next = low;
                // An overlap starts at a word boundary, never inside a code.
                while next < end
                    && !body[indices[next - 1]..indices[next]]
                        .chars()
                        .all(char::is_whitespace)
                {
                    next += 1;
                }
            }
            start = next.max(start + 1);
        }
        Ok(())
    }
}
pub(super) fn execute(
    text: &str,
    budget: usize,
    overlap: usize,
    search: &str,
    measure: Option<&Measure<'_>>,
) -> Result<Vec<Value>> {
    if length(search, measure)? >= budget {
        return Err("DOCUMENT_SEARCH_CONTEXT_BUDGET_EXCEEDED");
    }
    let mut builder = Builder {
        budget,
        overlap: overlap.min(budget.saturating_sub(1)),
        search,
        measure,
        parts: Vec::new(),
    };
    let units = crate::document::structure::units(text, 12000)?;
    for unit in units {
        let body = unit["body"].as_str().ok_or("DOCUMENT_STRUCTURE_INVALID")?;
        if unit["kind"] != "table_rows" {
            let context = unit["context_prefix"].as_str().unwrap_or("");
            builder.prose(context, body, false)?;
            continue;
        }
        let header = unit["table_header"].as_str().unwrap_or("");
        let heading = unit["heading"].as_str().unwrap_or("");
        let mut context = format!(
            "{}{header}",
            if heading.is_empty() {
                String::new()
            } else {
                format!("{heading}\n\n")
            }
        );
        let rows = body
            .strip_prefix(header)
            .filter(|_| !header.is_empty())
            .unwrap_or(body);
        let incomplete_context = builder.cost(&context, "")? >= budget;
        if incomplete_context {
            // Wide merged titles can be larger than the complete embedding
            // window. Preserve every literal header byte as search slices;
            // its data rows must be interpreted only after reading the source.
            builder.prose("", &context, true)?;
            context.clear();
        }
        let mut current = String::new();
        for row in rows.split_inclusive('\n') {
            if !current.is_empty() && builder.cost(&context, &format!("{current}{row}"))? > budget {
                builder.emit(&context, &current, incomplete_context)?;
                current.clear();
            }
            if builder.cost(&context, row)? > budget {
                builder.prose(&context, row, true)?;
            } else {
                current.push_str(row);
            }
        }
        if !current.is_empty() {
            builder.emit(&context, &current, incomplete_context)?;
        }
        // A header without body is still searchable and is not an empty INDEXED.
        if rows.trim().is_empty() && !header.is_empty() {
            builder.prose("", &context, false)?;
        }
    }
    Ok(builder.parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_table_slice_has_literal_captions_and_all_data_rows_survive() {
        let header = "|Position|Quantity|Mass|\n|---|---|---|\n";
        let rows = (0..40)
            .map(|i| format!("|Valve {i}|1|69.0|\n"))
            .collect::<Vec<_>>();
        let text = format!("# Equipment\n{header}{}", rows.join(""));
        let parts = execute(&text, 180, 30, "File: equipment.docx\n", None).unwrap();
        for row in &rows {
            assert_eq!(
                parts
                    .iter()
                    .filter(|p| p["text"].as_str().unwrap().contains(row.trim()))
                    .count(),
                1
            );
        }
        for part in parts
            .iter()
            .filter(|p| p["text"].as_str().unwrap().contains("Valve"))
        {
            assert!(part["text"].as_str().unwrap().contains(header.trim()));
            assert_eq!(part["requires_source_read"], false);
            assert!(part["length"].as_u64().unwrap() <= 180);
        }
    }
    #[test]
    fn long_rows_are_explicit_partial_reads_and_prefix_is_in_token_budget() {
        let text = format!(
            "|Name|Value|\n|---|---|\n|{}|17.25|\n",
            "unbroken ".repeat(50)
        );
        let parts = execute(&text, 120, 20, "Document: source\n", None).unwrap();
        assert!(parts.iter().any(|p| p["requires_source_read"] == true));
        assert!(parts.iter().all(|p| p["length"].as_u64().unwrap() <= 120));
        assert!(parts.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("17.25"));
    }
    #[test]
    fn a_header_larger_than_the_embedding_window_is_preserved_with_a_read_requirement() {
        let caption = "A very wide merged table title ".repeat(12);
        let text = format!("|{caption}|{caption}|\n|---|---|\n|Valve|17.25|\n");
        let parts = execute(&text, 120, 20, "Document: source\n", None).unwrap();
        assert!(parts.iter().all(|p| p["length"].as_u64().unwrap() <= 120));
        assert!(parts.iter().all(|p| p["requires_source_read"] == true));
        assert!(parts
            .iter()
            .any(|p| p["text"].as_str().unwrap().contains("|Valve|17.25|")));
        assert!(parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<String>()
            .contains("very wide merged table title"));
    }
}
