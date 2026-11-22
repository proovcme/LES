//! Reuse a model's citation when it identifies one physically read PDF row.
//! The receipt and the visible answer both come from that row.
use super::{decimal, Rows};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;

static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[+−-]?\p{Nd}+(?:[ \x{a0}\x{202f}]\p{Nd}{3})*(?:[.,]\p{Nd}+)?").unwrap()
});
static PARENTHETICAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^()\n]*\)").unwrap());

fn contains_cell(text: &str, cell: &str) -> bool {
    !cell.is_empty()
        && text.match_indices(cell).any(|(i, _)| {
            let part_of_word =
                |c: char| c.is_alphanumeric() || matches!(c, '_' | '/' | '·' | '%' | '°');
            !text[..i].ends_with(part_of_word) && !text[i + cell.len()..].starts_with(part_of_word)
        })
}

impl Rows {
    pub(in crate::chat) fn cited_pdf_row(
        &self,
        question: &str,
        answer: &str,
        sources: &Value,
    ) -> Result<Option<(String, Value, &Value)>, &'static str> {
        let refs = les_core::citations::execute(&json!({"answer":answer,"sources":sources}))?;
        let Some([id]) = refs["indexes"].as_array().map(Vec::as_slice) else {
            return Ok(None);
        };
        if refs["unknown_indexes"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        {
            return Ok(None);
        }
        let Some(index) = id.as_u64() else {
            return Ok(None);
        };
        let mut rows = self.data.range((index, 0)..=(index, u64::MAX));
        let Some((&(_, row), (cells, source))) = rows.next() else {
            return Ok(None);
        };
        if row == 0
            || rows.next().is_some()
            || !matches!(
                source["evidence_kind"].as_str(),
                Some("indexed_pdf_row" | "physical_pdf_unit")
            )
            || source["verified_cells"] != true
            || source["table_projection_warning"].is_string()
            || !source["source_sha256"].is_string()
            || !source["doc_id"].is_string()
        {
            return Ok(None);
        }
        // Only use a standalone, unchanged line. A whole page can contain several
        // tables, and an excerpt shortened for the model cannot certify a row.
        let line = format!("|{}|", cells.join("|"));
        if source["reading_unit_sha256"] != les_core::sha256_hex(line.as_bytes()) {
            return Ok(None);
        }
        let marker = format!("[Источник {index}]");
        let mut body = answer.to_owned();
        if let Some(header) = source["header"].as_str().filter(|h| !h.is_empty()) {
            body = body.replace(header, &marker);
        }
        if !body.contains(&marker) {
            return Ok(None);
        }
        let body = body.replace(&marker, "");
        let columns = &source["pdf_quantity_columns"];
        if body.contains(['[', ']']) || body.contains("```") {
            return Ok(None);
        }
        let Some((amount, unit)) = quantity(question, &body, cells, columns) else {
            return Ok(None);
        };
        // A second row answering the same literal question needs disambiguation.
        if self.data.iter().any(|(&(s, _), (other, origin))| {
            s != index && quantity(question, "", other, &origin["pdf_quantity_columns"]).is_some()
        }) {
            return Ok(None);
        }
        let mut verified = self.finish_value(
            &json!({"answerable":true,"rows":[format!("S{index}R{row}")],
            "sums":[],"missing":[]}),
            true,
        )?;
        if let Some((rendered, receipt)) = &mut verified {
            receipt["selection"] = json!("model_citation");
            receipt["rows"][0]["quantity_column"] = columns["quantity"].clone();
            receipt["rows"][0]["unit_column"] = columns["unit"].clone();
            let escape = les_core::chat_context::literal_markdown;
            *rendered = format!("{} {} {marker}", escape(amount), escape(unit));
        }
        Ok(verified.map(|(rendered, receipt)| (rendered, receipt, source)))
    }
}

/// A source header names both columns. A quantity has to match that column,
/// even if a different cell in the same row contains the number in the draft.
fn quantity<'a>(
    question: &str,
    answer: &str,
    cells: &'a [String],
    columns: &Value,
) -> Option<(&'a str, &'a str)> {
    if columns["width"].as_u64() != Some(cells.len() as u64) {
        return None;
    }
    let field = |name: &str| {
        cells.get(
            usize::try_from(columns[name].as_u64()?)
                .ok()?
                .checked_sub(1)?,
        )
    };
    let (Some(value), Some(unit)) = (field("quantity"), field("unit")) else {
        return None;
    };
    if unit.trim().is_empty() || decimal(unit).is_ok() {
        return None;
    }
    let Ok(amount) = decimal(value) else {
        return None;
    };
    let mut named = cells
        .iter()
        .filter(|c| *c != unit && decimal(c).is_err() && contains_cell(question, c));
    let (Some(item), None) = (named.next(), named.next()) else {
        return None;
    };
    if answer.trim().is_empty() {
        return Some((value, unit));
    }
    // An explanatory parenthesis may translate the unit. It cannot establish
    // the original unit, and numbers inside it still have to agree.
    let primary = PARENTHETICAL.replace_all(answer, " · ");
    if !contains_cell(&primary, item)
        || !contains_cell(&primary, unit)
        || !super::quantities::explicit_units_match(&primary, unit)
    {
        return None;
    }
    let body = answer.replace(item, "");
    let mut numbers = NUMBER.find_iter(&body).peekable();
    (numbers.peek().is_some() && numbers.all(|n| decimal(n.as_str()) == Ok(amount)))
        .then_some((value, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_supports_quantity(
        question: &str,
        answer: &str,
        cells: &[String],
        columns: &Value,
    ) -> bool {
        quantity(question, answer, cells, columns).is_some()
    }

    #[test]
    fn binds_only_the_named_item_and_literal_unit_with_one_reported_amount() {
        let cells = ["1", "Alpha item", "pcs", "17.25"].map(str::to_owned);
        let question = "Сколько Alpha item?";
        let columns = json!({"width":4,"quantity":4,"unit":3});
        for accepted in [
            "Alpha item 17,250 pcs",
            "",
            "Количество Alpha item **17,25** (или 17.25) штук. Единица — pcs.",
            "Alpha item **17,25** (или 17,25 штук). Единица — pcs (pieces).",
        ] {
            assert!(
                row_supports_quantity(question, accepted, &cells, &columns),
                "{accepted}"
            );
        }
        for rejected in [
            "Alpha item 999 pcs",
            "Alpha item 1 pcs",
            "Alpha item 17.25 kg",
            "Alpha item 17.25 kg (pcs)",
            "Alpha item 17.25 kg, оригинал pcs",
            "Alpha item 17.25 pcs/kg",
            "Beta item 17.25 pcs",
            "Alpha item 17.25 pcs (или 18)",
            "Alpha item pcs",
            "Alpha item 17.25",
        ] {
            assert!(
                !row_supports_quantity(question, rejected, &cells, &columns),
                "{rejected}"
            );
        }
        assert!(!row_supports_quantity(
            "How many Beta item?",
            "Alpha item 17.25 pcs",
            &cells,
            &columns
        ));
        let reordered = ["Alpha item", "17.25", "1", "pcs"].map(str::to_owned);
        let columns = json!({"width":4,"quantity":2,"unit":4});
        assert!(row_supports_quantity(
            question,
            "Alpha item 17.25 pcs",
            &reordered,
            &columns
        ));
        assert!(!row_supports_quantity(
            question,
            "Alpha item 1 pcs",
            &reordered,
            &columns
        ));
    }
}
