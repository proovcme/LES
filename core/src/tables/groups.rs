use super::{field, normalize, prepare};
use serde_json::{json, Value};

pub fn identifiers(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric() && !matches!(c, '-' | '‑' | '–'))
        .filter(|word| {
            word.chars().any(char::is_alphabetic) && word.bytes().any(|byte| byte.is_ascii_digit())
        })
        .map(|word| word.to_lowercase().replace(['‑', '–'], "-"))
}

#[derive(Default)]
pub(super) struct Groups {
    sheet: String,
    headings: Vec<Heading>,
}

struct Heading {
    position: String,
    row: u64,
    title: String,
}

impl Groups {
    // The artifact stores each sheet in physical row order.
    pub(super) fn observe(&mut self, row: &Value, wanted: bool) -> Option<Value> {
        let sheet = field(row, "sheet_name");
        if sheet != self.sheet {
            self.sheet = sheet.to_owned();
            self.headings.clear();
        }
        let coordinate = row["source_row"].as_u64()?;
        let mut position = field(row, "pos").trim();
        if let Some(integer) = position.strip_suffix(".0") {
            // A numeric Excel cell such as 1.0 is an integer position. A
            // textual "1.0" can be a distinct child position.
            if integer.bytes().all(|byte| byte.is_ascii_digit())
                && normalize::ordered(field(row, "raw_row"))
                    .iter()
                    .find(|(label, _)| prepare::column_target(label) == "pos")
                    .is_some_and(|(_, value)| value.is_number())
            {
                position = integer;
            }
        }
        if position.len() > 64
            || position
                .split('.')
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        self.headings.retain(|heading| {
            heading.row < coordinate
                && position
                    .strip_prefix(&heading.position)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        });
        let group = self.headings.last().filter(|_| wanted).map(
            |parent| json!({"position":parent.position,"row":parent.row,"title":parent.title}),
        );
        let title = field(row, "name").trim();
        let title = if title.is_empty() {
            field(row, "work_name").trim()
        } else {
            title
        };
        if !title.is_empty() {
            self.headings.push(Heading {
                position: position.to_owned(),
                row: coordinate,
                title: title.chars().take(320).collect(),
            });
        }
        group
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_ancestry_follows_physical_order_within_a_sheet() {
        let mut groups = Groups::default();
        let row = |sheet: &str, n, pos: &str, title: &str, raw: &str| json!({"sheet_name":sheet,"source_row":n,"pos":pos,"name":title,"raw_row":raw});
        groups.observe(&row("A", 3, "1.0", "Assembly A", r#"{"Поз.":1.0}"#), false);
        groups.observe(&row("A", 4, "1.1", "Part A", ""), false);
        assert_eq!(
            groups.observe(&row("A", 8, "1.10", "Part B", ""), true),
            Some(json!({"position":"1","row":3,"title":"Assembly A"}))
        );
        groups.observe(
            &row("A", 9, "1.0", "Subassembly", r#"{"Поз.":"1.0"}"#),
            false,
        );
        assert_eq!(
            groups
                .observe(&row("A", 10, "1.0.1", "Part C", ""), true)
                .unwrap()["row"],
            9
        );
        groups.observe(&row("A", 11, "2", "Assembly B", ""), false);
        assert!(groups
            .observe(&row("A", 12, "1.5", "Part D", ""), true)
            .is_none());
        assert!(groups
            .observe(&row("B", 8, "1.10", "Part E", ""), true)
            .is_none());
    }
}
