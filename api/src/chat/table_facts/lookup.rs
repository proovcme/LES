//! The caller supplies a literal from the question. The model selects headers,
//! not cell values. Only rows admitted into the current evidence packet qualify.
use super::Rows;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) struct Lookup {
    key: String,
    headers: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_record_and_fields_use_physical_columns_and_preserve_empty_and_zero() {
        let block = json!({"record_kind":"spreadsheet_row","canonical":{"kind":"table","header":["Наименование","Ед. изме-\nрения","Кол."]}});
        let lookup = Lookup::from_blocks("Количество и единица «Датчик А»?", &[block]).unwrap();
        let mut packet = Rows::default();
        let key = (9, 1);
        let origin = json!({"evidence_kind":"physical_workbook_row","sheet_name":"Лист","row":18,"doc_id":"d","source_sha256":"rev"});
        packet.headers.insert(
            key,
            vec![
                "Наименование".into(),
                "Ед. изме-\nрения".into(),
                "Кол.".into(),
            ],
        );
        packet.data.insert(
            key,
            (vec!["Датчик А".into(), "".into(), "0".into()], origin),
        );
        let plan = r#"{"fields":["Кол.","Ед. измерения"]}"#;
        let (answer, receipt) = lookup.answer(plan, &packet).unwrap();
        assert_eq!(
            receipt["rows"][0]["fields"][0],
            json!({"header":"Кол.","column":3,"value":"0"})
        );
        assert_eq!(receipt["rows"][0]["row"], 18);
        assert_eq!(receipt["status"], "partial");
        assert!(answer.contains("нет значения") && answer.contains("[Источник 9]"));
        assert!(lookup
            .answer(r#"{"fields":["выдумано"]}"#, &packet)
            .is_none());
        packet.data.insert((2, 1), packet.data[&key].clone());
        assert_eq!(
            lookup.answer(plan, &packet).unwrap().1["status"],
            "insufficient"
        );
        packet.data.clear();
        assert_eq!(
            lookup.answer(plan, &packet).unwrap().1["status"],
            "insufficient"
        );
    }
}

fn label(text: &str) -> String {
    // Printed headers often wrap within a word. Retain ordinary inline hyphens.
    text.replace("-\r\n", "")
        .replace("-\n", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn literal(question: &str) -> Option<&str> {
    for (open, close) in [('«', '»'), ('"', '"'), ('“', '”')] {
        let Some((prefix, rest)) = question.split_once(open) else {
            continue;
        };
        let (value, suffix) = rest.split_once(close)?;
        if !value.trim().is_empty()
            && ![prefix, value, suffix]
                .iter()
                .any(|s| s.contains(['«', '»', '“', '”', '"']))
        {
            return Some(value.trim());
        }
    }
    None
}

impl Lookup {
    pub(in crate::chat) fn named_record(question: &str) -> bool {
        literal(question).is_some()
    }
    pub(crate) fn from_blocks(question: &str, blocks: &[Value]) -> Option<Self> {
        let key = literal(question)?.to_owned();
        let mut headers = Vec::new();
        for block in blocks {
            let table = &block["canonical"];
            if block["record_kind"] != "spreadsheet_row" || table["kind"] != "table" {
                return None;
            }
            for h in table["header"].as_array()? {
                let name = label(h.as_str()?);
                if !name.is_empty() && !headers.contains(&name) {
                    headers.push(name);
                }
            }
        }
        (headers.len() <= 128 && !headers.is_empty()).then_some(Self { key, headers })
    }

    pub(crate) fn instruction(&self) -> String {
        format!("Выбери из списка заголовки столбцов, значения которых пользователь хочет получить. Название искомой позиции уже используется для выбора строки. Верни JSON с полем fields — массивом нужных заголовков. Для расчёта, обзора, сравнения или вопроса о нескольких условиях верни пустой массив.\nЗаголовки: {}", json!(self.headers))
    }

    pub(crate) fn format(&self) -> Value {
        json!({"type":"json_schema","json_schema":{"name":"workbook_fields","strict":true,"schema":{
            "type":"object","properties":{"fields":{"type":"array","maxItems":16,"items":{"type":"string","enum":self.headers}}},
            "required":["fields"],"additionalProperties":false}}})
    }

    pub(crate) fn answer(&self, response: &str, packet: &Rows) -> Option<(String, Value)> {
        let plan: Value = serde_json::from_str(super::json_answer(response)).ok()?;
        let fields = plan["fields"].as_array()?;
        if plan.as_object()?.len() != 1 || fields.is_empty() || fields.len() > 16 {
            return None;
        }
        let mut selected = Vec::new();
        for field in fields {
            let field = field.as_str()?;
            if !self.headers.iter().any(|h| h == field) {
                return None;
            }
            if !selected.contains(&field) {
                selected.push(field)
            }
        }
        let normalize = |v: &str| {
            v.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let key = normalize(&self.key);
        let mut matches = packet.data.iter().filter(|(_, (cells, source))| {
            source["evidence_kind"] == "physical_workbook_row"
                && cells.iter().any(|c| normalize(c) == key)
        });
        let empty = |message: &str| {
            (
                message.to_owned(),
                json!({"schema":"les.table-facts.v1","status":"insufficient","rows":[],"sums":[],"operation":"lookup"}),
            )
        };
        let Some((id, (cells, source))) = matches.next() else {
            return Some(empty(
                "В прочитанных строках точного совпадения с указанным названием нет.",
            ));
        };
        if matches.next().is_some() {
            return Some(empty("В прочитанных таблицах найдено несколько строк с таким значением. Уточните книгу, лист или строку."));
        }
        let headers = packet.headers.get(id)?;
        let row = source["row"].as_u64()?;
        let sheet = source["sheet_name"].as_str()?;
        let mut rendered = Vec::new();
        let mut projected = Vec::new();
        let mut missing = HashSet::new();
        for field in selected {
            let mut indexes = headers
                .iter()
                .enumerate()
                .filter(|(_, h)| label(h) == field);
            let column = indexes.next().map(|(c, _)| c);
            if indexes.next().is_some() {
                return Some(empty("В найденной строке несколько столбцов с одинаковой подписью. Уточните адрес столбца."));
            }
            let value = column
                .and_then(|c| cells.get(c))
                .filter(|v| !v.trim().is_empty());
            if value.is_none() {
                missing.insert(field);
            }
            let display = value
                .map(|v| les_core::chat_context::literal_markdown(v))
                .unwrap_or_else(|| "нет значения".into());
            rendered.push(format!(
                "{} — {} [Источник {}]",
                les_core::chat_context::literal_markdown(field),
                display,
                id.0
            ));
            projected.push(json!({"header":field,"column":column.map(|c|c+1),"value":value}));
        }
        Some((
            rendered.join("\n\n"),
            json!({"schema":"les.table-facts.v1","operation":"lookup",
            "status":if missing.is_empty(){"verified"}else{"partial"},"sums":[],
            "rows":[{"kind":"table_row","source_index":id.0,"row":row,"sheet":sheet,"cells":cells,"fields":projected,
            "doc_id":source["doc_id"],"source_sha256":source["source_sha256"]}]}),
        ))
    }
}
