//! The model selects visible rows. Native code copies cells and computes sums.
//! This proves source binding, not relevance of the selected row to the question.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
mod addresses;
use addresses::Reference;
#[derive(Default)]
pub(super) struct Rows {
    data: BTreeMap<(u64, u64), (Vec<String>, Value)>,
    paragraphs: BTreeMap<(u64, u64), (Vec<String>, Value)>,
    headers: BTreeMap<(u64, u64), Vec<String>>,
}
pub(super) const CONTRACT:&str="Финальный ответ по источникам: верни только JSON {\"answerable\":false,\"rows\":[],\"sums\":[]}. Сначала реши, есть ли в тексте именно запрошенные сведения для нужного предмета. Совпадение наименования строки ещё не подтверждает наличие показателя. Если сведений нет, answerable=false, rows и sums пустые. При наличии подтверждения поставь answerable=true и заполни rows скопированными адресами: S…R… для строк таблиц, S…P… для текстовых абзацев, S… для явно помеченного целого фрагмента. Не переписывай значения: Лес скопирует оригинал и добавит ссылки. Пояснения до и после таблицы доступны отдельно по S…P…, не заменяй их строками таблицы. Для справки выбери относящиеся к вопросу абзацы и строки в связном порядке. Не выбирай строку только за совпадение названия: количество, масса и другие поля не заменяют отсутствующий показатель. Физический номер строки Excel не является адресом S…R…. Только для запрошенной суммы табличных ячеек заполни sums объектами с column (номер C) и unit_column (номер C либо null). Текстовые абзацы и целые фрагменты не являются ячейками для сложения. Без суммы оставь sums пустым. Данные другого вида, стадии или редакции документа не подтверждают условия вопроса.";
pub(super) fn instructions(can_refine: bool) -> String {
    if can_refine {
        format!("Проверь соответствие источника всем условиям вопроса: предмет, вид документа, редакция и запрошенный показатель. Успешное чтение или совпадение числа этого не доказывают. Карточки candidate_documents помогают найти подходящий файл за пределами первых прочитанных совпадений. Если нужного подтверждения в тексте нет, сначала вызови reader с его doc_id либо уточни поиск. Если подтверждение не найдено, rows остаётся пустым. JSON-договор относится к финальному ответу, а не к выбору инструмента.\n{CONTRACT}")
    } else {
        CONTRACT.to_owned()
    }
}
pub(super) fn quote_sources(sources: &mut Value, receipt: &Value) {
    for source in sources.as_array_mut().into_iter().flatten() {
        let quotes = receipt["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                r["source_index"] == source["index"]
                    && r["doc_id"] == source["doc_id"]
                    && r["source_sha256"] == source["source_sha256"]
            })
            .filter_map(|r| {
                r["cells"].as_array().map(|cells| {
                    if r["kind"] == "source_excerpt" {
                        cells[0].as_str().unwrap_or("").to_owned()
                    } else {
                        format!(
                            "|{}|",
                            cells
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join("|")
                        )
                    }
                })
            })
            .collect::<Vec<_>>();
        if !quotes.is_empty() {
            source["quote"] = json!(quotes.join("\n"));
        }
    }
}
impl Rows {
    fn get(&self, reference: Reference) -> Option<&(Vec<String>, Value)> {
        match reference {
            Reference::Paragraph(..) => self.paragraphs.get(&reference.key()),
            _ => self.data.get(&reference.key()),
        }
    }
    pub(super) fn format(&self, native: bool) -> Value {
        let ids = self
            .data
            .keys()
            .map(|(s, r)| {
                if *r == 0 {
                    format!("S{s}")
                } else {
                    format!("S{s}R{r}")
                }
            })
            .chain(
                self.paragraphs
                    .keys()
                    .map(|(s, line)| Reference::Paragraph(*s, *line).label()),
            )
            .collect::<Vec<_>>();
        let mut item = json!({"type":"string"});
        if native && !ids.is_empty() && ids.len() <= 512 {
            item["enum"] = json!(ids);
        }
        // A paragraph is an excerpt, not a numeric cell. Do not offer an
        // arithmetic schema when none of the selectable cells is numeric.
        let columns: BTreeSet<_> = self
            .data
            .iter()
            .filter(|((_, row), _)| *row > 0)
            .flat_map(|(_, (cells, _))| {
                cells
                    .iter()
                    .enumerate()
                    .filter_map(|(column, cell)| decimal(cell).is_ok().then_some(column + 1))
            })
            .collect();
        let width = self
            .data
            .values()
            .map(|(cells, _)| cells.len())
            .max()
            .unwrap_or(1)
            .max(1);
        let mut column = json!({"type":"integer","minimum":1,"maximum":width});
        if !columns.is_empty() {
            column["enum"] = json!(columns);
        }
        json!({"type":"json_schema","json_schema":{"name":"les_source_rows","strict":true,"schema":{
        "type":"object","additionalProperties":false,"required":["answerable","rows","sums"],"properties":{
            "answerable":{"type":"boolean"},
            "rows":{"type":"array","maxItems":128,"items":item},
            "sums":{"type":"array","maxItems":if columns.is_empty(){0}else{16},"items":{"type":"object","additionalProperties":false,"required":["column","unit_column"],"properties":{"column":column,"unit_column":{"type":["integer","null"],"minimum":1,"maximum":width}}}}
        }}}})
    }
    pub(super) fn supported(blocks: &[Value]) -> bool {
        blocks.iter().any(|b| {
            (b["schema"] == "les.evidence-unit.v1"
                && b["blocks"]
                    .as_array()
                    .is_some_and(|rows| Self::supported(rows)))
                || b["record_kind"] == "spreadsheet_row"
                || !les_core::chat_context::table_rows(b["text"].as_str().unwrap_or("")).is_empty()
        })
    }
    pub(super) fn present(&self) -> bool {
        !self.data.is_empty() || !self.paragraphs.is_empty()
    }
    pub(super) fn new(packet: &Value, sources: &Value) -> Self {
        fn visit(value: &Value, sources: &Value, out: &mut Rows) {
            if value["schema"] == "les.evidence-unit.v1" {
                for b in value["blocks"].as_array().into_iter().flatten() {
                    visit(b, sources, out);
                }
                return;
            }
            if value["schema"] != "les.evidence-block.v1" {
                return;
            }
            let Some(index) = value["source_index"].as_u64() else {
                return;
            };
            let Some(source) = sources
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["index"] == index)
            else {
                return;
            };
            let text = value["text"].as_str().unwrap_or("");
            if let Some(offset) = value["body_offset"].as_u64() {
                for (line, paragraph) in les_core::chat_context::prose_spans(text, offset as usize)
                {
                    out.paragraphs
                        .insert((index, line), (vec![paragraph], source.clone()));
                }
            }
            if let Some(body) = value["body_offset"]
                .as_u64()
                .and_then(|n| text.get(n as usize..))
                .filter(|s| {
                    !s.trim().is_empty() && les_core::chat_context::table_rows(s).is_empty()
                })
            {
                out.data
                    .insert((index, 0), (vec![body.to_owned()], source.clone()));
            }
            let headers = les_core::chat_context::table_headers(text);
            for (row, cells) in
                les_core::chat_context::table_rows(value["text"].as_str().unwrap_or(""))
            {
                let header = headers.iter().rev().find(|(h, labels)| {
                    *h < row
                        && labels.len() == cells.len()
                        && text
                            .lines()
                            .skip(*h as usize)
                            .take((row - h - 1) as usize)
                            .all(|l| {
                                let l = l.trim();
                                l.starts_with('|') && l.ends_with('|')
                            })
                });
                if let Some((_, labels)) = header {
                    out.headers.insert((index, row), labels.clone());
                }
                out.data.insert((index, row), (cells, source.clone()));
            }
        }
        let mut out = Self::default();
        for section in packet["sections"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| s["kind"] == "evidence")
        {
            for object in section["objects"].as_array().into_iter().flatten() {
                visit(&object["payload"], sources, &mut out);
            }
        }
        out
    }
    #[cfg(test)]
    fn finish(&self, answer: &str) -> Result<Option<(String, Value)>, &'static str> {
        self.finish_checked(answer, false)
    }
    pub(super) fn finish_checked(
        &self,
        answer: &str,
        require_decision: bool,
    ) -> Result<Option<(String, Value)>, &'static str> {
        if !self.present() {
            return Ok(None);
        }
        let text = answer
            .trim()
            .strip_prefix("```json")
            .or_else(|| answer.trim().strip_prefix("```"))
            .map(|s| s.trim().trim_end_matches("```").trim())
            .unwrap_or(answer.trim());
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => {
                if require_decision {
                    return Err("TABLE_FACTS_REQUIRED");
                }
                let mut rest = answer;
                let mut prose = String::new();
                while let Some(start) = rest.find("[Источник") {
                    prose.push_str(&rest[..start]);
                    rest = &rest[start..];
                    if let Some(end) = rest.find(']') {
                        rest = &rest[end + 1..];
                    } else {
                        break;
                    }
                }
                prose.push_str(rest);
                return if prose.chars().any(char::is_numeric) {
                    Err("TABLE_FACTS_REQUIRED")
                } else {
                    Ok(None)
                };
            }
        };
        let object = value.as_object().ok_or("TABLE_FACTS_REQUIRED")?;
        if object
            .keys()
            .any(|k| !["answerable", "rows", "sums"].contains(&k.as_str()))
        {
            return Err("TABLE_FACTS_REQUIRED");
        }
        let decision = value
            .get("answerable")
            .map(|v| v.as_bool().ok_or("TABLE_FACTS_REQUIRED"))
            .transpose()?;
        if require_decision && decision.is_none() {
            return Err("TABLE_FACTS_REQUIRED");
        }
        let refs = |key: &str, required: bool| -> Result<Vec<Reference>, &'static str> {
            let Some(rows) = value[key].as_array() else {
                return if required || value.get(key).is_some() {
                    Err("TABLE_FACTS_REQUIRED")
                } else {
                    Ok(vec![])
                };
            };
            if rows.len() > 128 {
                return Err("TABLE_ANSWER_TOO_LARGE");
            }
            let mut seen = HashSet::new();
            let mut out = Vec::new();
            for r in rows {
                let id = Reference::parse(r.as_str().ok_or("TABLE_FACTS_REQUIRED")?)?;
                if self.get(id).is_none() {
                    return Err("TABLE_ROW_NOT_VISIBLE");
                }
                if seen.insert(id) {
                    out.push(id);
                }
            }
            Ok(out)
        };
        let selected = refs("rows", true)?;
        if decision.is_some_and(|answerable| answerable == selected.is_empty()) {
            return Err("TABLE_FACTS_REQUIRED");
        }
        if selected.is_empty() {
            if value
                .get("sums")
                .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
            {
                return Err("TABLE_FACTS_REQUIRED");
            }
            let indexes = self
                .data
                .keys()
                .map(|(s, _)| *s)
                .collect::<std::collections::BTreeSet<_>>();
            let citations = indexes
                .iter()
                .take(16)
                .map(|n| format!("[Источник {n}]"))
                .collect::<Vec<_>>()
                .join(" ");
            return Ok(Some((format!("В прочитанных фрагментах не удалось подтвердить запрошенные показатели. Нужен подходящий документ или дополнительное чтение. {citations}"),json!({"schema":"les.table-facts.v1","status":"insufficient","rows":[],"sums":[]}))));
        }
        let mut rendered = String::new();
        let mut receipt = Vec::new();
        let mut previous_header = Vec::new();
        for id in &selected {
            let key = id.key();
            let (cells, source) = self.get(*id).ok_or("TABLE_ROW_NOT_VISIBLE")?;
            if matches!(id, Reference::Paragraph(..)) {
                if !rendered.is_empty() {
                    rendered.push('\n');
                }
                rendered.push_str(&format!("{}\n\n[Источник {}]\n", cells[0], key.0));
                previous_header.clear();
                receipt.push(json!({"source_index":key.0,"row":null,"paragraph_line":key.1,"address":id.label(),
                    "kind":"source_excerpt","cells":cells,"doc_id":source["doc_id"],"source_sha256":source["source_sha256"]}));
                continue;
            }
            // A header must be explicit Markdown structure in this very table,
            // never another data row selected by the model or a different page.
            let (names, display_cells) = if let Some(header) = self.headers.get(&key) {
                (header.clone(), cells.clone())
            } else {
                // Preserve the source row without inventing column meanings.
                // Technical C-addresses belong to model input, not the user answer.
                (
                    vec!["Строка документа".to_owned()],
                    vec![cells
                        .iter()
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" · ")],
                )
            };
            let escape = |v: &str| v.replace('|', "\\|").replace('\n', "<br>");
            if names != previous_header {
                if !rendered.is_empty() {
                    rendered.push('\n');
                }
                rendered.push_str(&format!(
                    "|{}|Источник|\n|{}|---|\n",
                    names
                        .iter()
                        .map(|s| escape(s))
                        .collect::<Vec<_>>()
                        .join("|"),
                    vec!["---"; names.len()].join("|")
                ));
                previous_header = names;
            }
            rendered.push_str(&format!(
                "|{}|[Источник {}]|\n",
                display_cells
                    .iter()
                    .map(|s| escape(s))
                    .collect::<Vec<_>>()
                    .join("|"),
                key.0
            ));
            receipt.push(json!({"source_index":key.0,"row":if key.1==0{Value::Null}else{json!(key.1)},"kind":if key.1==0{"source_excerpt"}else{"table_row"},"cells":cells,"doc_id":source["doc_id"],"source_sha256":source["source_sha256"]}));
        }
        let mut sums = Vec::new();
        if value.get("sums").is_some() && !value["sums"].is_array() {
            return Err("TABLE_FACTS_REQUIRED");
        }
        if let Some(operations) = value["sums"].as_array() {
            if operations.len() > 16 {
                return Err("TABLE_ANSWER_TOO_LARGE");
            }
            for op in operations {
                if op.as_object().is_none_or(|v| {
                    v.keys()
                        .any(|k| !["column", "unit_column"].contains(&k.as_str()))
                }) {
                    return Err("TABLE_VALUE_INVALID");
                }
                let column = op["column"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("TABLE_VALUE_INVALID")? as usize
                    - 1;
                let unit = if op.get("unit_column").is_some() && !op["unit_column"].is_null() {
                    Some(
                        op["unit_column"]
                            .as_u64()
                            .filter(|n| *n > 0)
                            .ok_or("TABLE_VALUE_INVALID")? as usize
                            - 1,
                    )
                } else {
                    None
                };
                let mut total = 0_i128;
                let mut units = HashSet::new();
                let mut values = Vec::new();
                for id in &selected {
                    if !matches!(id, Reference::Row(..)) {
                        return Err("TABLE_VALUE_INVALID");
                    }
                    let cells = &self.get(*id).ok_or("TABLE_ROW_NOT_VISIBLE")?.0;
                    let raw = cells.get(column).ok_or("TABLE_VALUE_INVALID")?;
                    total = total
                        .checked_add(decimal(raw)?)
                        .ok_or("TABLE_VALUE_INVALID")?;
                    values.push(raw.clone());
                    if let Some(unit) = unit {
                        units.insert(
                            cells
                                .get(unit)
                                .ok_or("TABLE_VALUE_INVALID")?
                                .trim()
                                .to_owned(),
                        );
                    }
                }
                if units.len() > 1 {
                    return Err("TABLE_UNITS_MISMATCH");
                }
                let result = display(total);
                let unit = units.into_iter().next().unwrap_or_default();
                rendered.push_str(&format!(
                    "\nСумма выбранных строк по C{}: **{} {}**.\n",
                    column + 1,
                    result,
                    unit
                ));
                sums.push(json!({"column":column+1,"operands":values,"result":result,"unit":unit}));
            }
        }
        if rendered.chars().count() > 16000 {
            return Err("TABLE_ANSWER_TOO_LARGE");
        }
        Ok(Some((
            rendered.trim().to_owned(),
            json!({"schema":"les.table-facts.v1","status":"copied","rows":receipt,"sums":sums}),
        )))
    }
}
const SCALE: i128 = 1_000_000_000_000;
fn decimal(value: &str) -> Result<i128, &'static str> {
    let text = value.trim().replace(',', ".").replace('−', "-");
    let neg = text.starts_with('-');
    let text = if text.starts_with(['-', '+']) {
        &text[1..]
    } else {
        &text
    };
    let parts = text.split('.').collect::<Vec<_>>();
    if parts.len() > 2 || parts[0].is_empty() {
        return Err("TABLE_VALUE_INVALID");
    }
    let groups = parts[0]
        .split([' ', '\u{a0}', '\u{202f}'])
        .collect::<Vec<_>>();
    if groups
        .iter()
        .any(|g| g.is_empty() || !g.bytes().all(|c| c.is_ascii_digit()))
        || (groups.len() > 1
            && (groups[0].len() > 3 || groups.iter().skip(1).any(|g| g.len() != 3)))
    {
        return Err("TABLE_VALUE_INVALID");
    }
    let fraction = parts.get(1).copied().unwrap_or("");
    if fraction.len() > 12 || !fraction.bytes().all(|c| c.is_ascii_digit()) {
        return Err("TABLE_VALUE_INVALID");
    }
    let whole = groups
        .join("")
        .parse::<i128>()
        .map_err(|_| "TABLE_VALUE_INVALID")?
        .checked_mul(SCALE)
        .ok_or("TABLE_VALUE_INVALID")?;
    let tail = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| "TABLE_VALUE_INVALID")?
            * 10_i128.pow(12 - fraction.len() as u32)
    };
    let total = whole.checked_add(tail).ok_or("TABLE_VALUE_INVALID")?;
    if neg {
        total.checked_neg().ok_or("TABLE_VALUE_INVALID")
    } else {
        Ok(total)
    }
}
fn display(value: i128) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let value = value.unsigned_abs();
    let whole = value / (SCALE as u128);
    let tail = format!("{:012}", value % (SCALE as u128));
    let tail = tail.trim_end_matches('0');
    format!(
        "{sign}{whole}{}",
        if tail.is_empty() {
            String::new()
        } else {
            format!(".{tail}")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_evidence_keeps_selectable_prose_and_numeric_cells_separate() {
        let prefix = "[Source 1]\n{}\n";
        let body = "Before: 17.35 kW.\n\n|Name|Qty|\n|---|---|\n|Pump|29|\n\nAfter: 42.75 m.";
        let text = format!("{prefix}{body}");
        let block = json!({"schema":"les.evidence-block.v1","source_index":1,"body_offset":prefix.len(),"text":text});
        let unit = json!({"schema":"les.evidence-unit.v1","blocks":[block]});
        assert!(Rows::supported(&[unit.clone()]));
        let packet = json!({"sections":[{"kind":"evidence","objects":[{"payload":unit}]}]});
        let sources = json!([{"index":1,"doc_id":"doc","source_sha256":"r1"}]);
        let rows = Rows::new(&packet, &sources);
        let ids =
            &rows.format(true)["json_schema"]["schema"]["properties"]["rows"]["items"]["enum"];
        for id in ["S1P3", "S1R7", "S1P9"] {
            assert!(ids.as_array().unwrap().contains(&json!(id)));
        }
        let (text, proof) = rows
            .finish_checked(r#"{"answerable":true,"rows":["S1P3"],"sums":[]}"#, true)
            .unwrap()
            .unwrap();
        assert_eq!(text, "Before: 17.35 kW.\n\n[Источник 1]");
        assert_eq!(proof["rows"][0]["paragraph_line"], 3);
        assert_eq!(proof["rows"][0]["kind"], "source_excerpt");
        assert_eq!(proof["rows"][0]["cells"], json!(["Before: 17.35 kW."]));
        assert!(!text.contains("Pump") && !text.contains("29") && !text.contains("42.75"));
        for id in ["S1P7", "S1R3", "S2P3"] {
            assert_eq!(
                rows.finish_checked(
                    &json!({"answerable":true,"rows":[id],"sums":[]}).to_string(),
                    true
                )
                .unwrap_err(),
                "TABLE_ROW_NOT_VISIBLE"
            );
        }
        assert_eq!(
            rows.finish(r#"{"rows":["S1P3"],"sums":[{"column":1,"unit_column":null}]}"#)
                .unwrap_err(),
            "TABLE_VALUE_INVALID"
        );
        let mut rows = rows;
        rows.data.insert((2, 0), (vec!["99".into()], Value::Null));
        assert_eq!(
            rows.finish(r#"{"rows":["S2"],"sums":[{"column":1,"unit_column":null}]}"#)
                .unwrap_err(),
            "TABLE_VALUE_INVALID"
        );
    }
    #[test]
    fn arithmetic_schema_only_offers_visible_numeric_columns() {
        let mut rows = sample();
        // A numeric paragraph is still an excerpt, not a table operand.
        rows.data.insert((2, 0), (vec!["99".into()], Value::Null));
        let schema = rows.format(true);
        let operation = &schema["json_schema"]["schema"]["properties"]["sums"]["items"];
        assert_eq!(operation["properties"]["column"]["enum"], json!([3]));
        assert_eq!(operation["properties"]["column"]["maximum"], 3);
        assert_eq!(operation["properties"]["unit_column"]["maximum"], 3);
        assert_eq!(
            rows.finish(r#"{"rows":["S1R4"],"sums":[{"column":7,"unit_column":null}]}"#)
                .unwrap_err(),
            "TABLE_VALUE_INVALID"
        );
        let (_, receipt) = rows
            .finish(r#"{"rows":["S1R4","S1R5"],"sums":[{"column":3,"unit_column":2}]}"#)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["sums"][0]["result"], "0.3");
    }
    #[test]
    fn a_missing_indicator_requires_abstention_not_a_matching_item_row() {
        let rows = sample();
        assert_eq!(
            rows.finish_checked(r#"{"rows":["S1R4"]}"#, true)
                .unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
        assert_eq!(
            rows.finish_checked(r#"{"answerable":false,"rows":["S1R4"]}"#, true)
                .unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
        assert_eq!(
            rows.finish_checked(r#"{"answerable":true,"rows":[]}"#, true)
                .unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
        let (answer, proof) = rows
            .finish_checked(r#"{"answerable":false,"rows":[],"sums":[]}"#, true)
            .unwrap()
            .unwrap();
        assert_eq!(proof["status"], "insufficient");
        assert!(!answer.contains("Alpha") && !answer.contains("0.1"));
        assert!(rows
            .finish_checked(r#"{"answerable":true,"rows":["S1R4"],"sums":[]}"#, true)
            .unwrap()
            .is_some());
    }
    fn sample() -> Rows {
        let source =
            json!({"index":1,"doc_id":"doc","dataset_id":"ds","source_sha256":"a".repeat(64)});
        let data = [
            ((1, 3), vec!["Item", "Unit", "Quantity"]),
            ((1, 4), vec!["Alpha", "m", "0.1"]),
            ((1, 5), vec!["Beta", "m", "0.2"]),
        ]
        .into_iter()
        .map(|(id, cells)| {
            (
                id,
                (
                    cells.into_iter().map(str::to_owned).collect(),
                    source.clone(),
                ),
            )
        })
        .collect();
        Rows {
            data,
            paragraphs: BTreeMap::new(),
            headers: BTreeMap::from([(
                (1, 4),
                vec!["Item".into(), "Unit".into(), "Quantity".into()],
            )]),
        }
    }
    #[test]
    fn source_rows_supply_the_answer_instead_of_model_numbers() {
        let rows = sample();
        let (limited, receipt) = rows.finish(r#"{"rows":[],"sums":[]}"#).unwrap().unwrap();
        assert_eq!(receipt["status"], "insufficient");
        assert!(limited.contains("В прочитанных фрагментах") && limited.contains("[Источник 1]"));
        let schema = rows.format(true);
        assert!(
            schema["json_schema"]["schema"]["properties"]["rows"]["items"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("S1R4"))
        );
        assert!(
            rows.format(false)["json_schema"]["schema"]["properties"]["rows"]["items"]
                .get("enum")
                .is_none()
        );
        assert_eq!(
            rows.finish("273 m [Источник 1]").unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
        let (answer, proof) = rows.finish(r#"{"rows":["S1R4"]}"#).unwrap().unwrap();
        assert!(answer.contains("Alpha|m|0.1|[Источник 1]"));
        assert_eq!(proof["rows"][0]["cells"][2], "0.1");
        assert_eq!(
            rows.finish(r#"{"rows":["S2R4"]}"#).unwrap_err(),
            "TABLE_ROW_NOT_VISIBLE"
        );
        assert_eq!(
            rows.finish(r#"{"rows":["S1R4"],"answer":"999"}"#)
                .unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
    }
    #[test]
    fn sums_are_decimal_deduplicated_and_do_not_treat_empty_as_zero() {
        let mut rows = sample();
        let (_, proof) = rows
            .finish(r#"{"rows":["S1R4","S1R5","S1R4"],"sums":[{"column":3,"unit_column":2}]}"#)
            .unwrap()
            .unwrap();
        assert_eq!(proof["sums"][0]["result"], "0.3");
        assert_eq!(proof["rows"].as_array().unwrap().len(), 2);
        rows.data.get_mut(&(1, 5)).unwrap().0[1] = "pcs".into();
        assert_eq!(
            rows.finish(r#"{"rows":["S1R4","S1R5"],"sums":[{"column":3,"unit_column":2}]}"#)
                .unwrap_err(),
            "TABLE_UNITS_MISMATCH"
        );
        rows.data.get_mut(&(1, 5)).unwrap().0[2] = String::new();
        assert_eq!(
            rows.finish(r#"{"rows":["S1R4","S1R5"],"sums":[{"column":3}]}"#)
                .unwrap_err(),
            "TABLE_VALUE_INVALID"
        );
        assert!(decimal("--1").is_err());
        assert!(decimal("1 2").is_err() && decimal("1\n2").is_err());
        assert_eq!(display(decimal("1\u{a0}000,05").unwrap()), "1000.05");
        assert_eq!(display(decimal("-17,25").unwrap()), "-17.25");
    }
    #[test]
    fn headers_are_structural_and_never_borrowed_across_prose_or_pages() {
        let block = |index, text| json!({"schema":"les.evidence-block.v1","source_index":index,"text":text});
        let packet = json!({"sections":[{"kind":"evidence","objects":[
            {"payload":block(1,"|Name|Count|\n|---|---|\n|Alpha|17.25|\nNew table without declared header\n|Beta|23.5|")},
            {"payload":block(2,"|Gamma|4|")}]}]});
        let rows = Rows::new(&packet, &json!([{"index":1},{"index":2}]));
        let (answer, _) = rows
            .finish(r#"{"rows":["S1R3","S1R5","S2R1"]}"#)
            .unwrap()
            .unwrap();
        assert!(answer.contains("|Name|Count|Источник|"));
        assert!(answer.contains("|Строка документа|Источник|"));
        assert!(
            answer.contains("|Beta · 23.5|[Источник 1]|")
                && answer.contains("|Gamma · 4|[Источник 2]|")
        );
        assert!(!rows.headers.contains_key(&(1, 5)) && !rows.headers.contains_key(&(2, 1)));
        assert_eq!(
            rows.finish(r#"{"rows":["S1R3"],"headers":["S1R5"]}"#)
                .unwrap_err(),
            "TABLE_FACTS_REQUIRED"
        );
    }
    #[test]
    fn mixed_excel_text_uses_its_bound_excerpt_not_a_guessed_physical_row_address() {
        let quote = "Name: Cabinet; Unit: pcs; Quantity: 1; Mass: 65 kg";
        let text = format!("[Source 1]\n{{\"row\":57}}\n{quote}");
        let packet = json!({"sections":[{"kind":"evidence","objects":[{"payload":{
            "schema":"les.evidence-block.v1","record_kind":"spreadsheet_row","source_index":1,"body_offset":text.len()-quote.len(),"text":text}}]}]});
        assert!(Rows::supported(&[packet["sections"][0]["objects"][0]
            ["payload"]
            .clone()]));
        let rows = Rows::new(&packet, &json!([{"index":1,"row":57}]));
        let (answer, proof) = rows.finish(r#"{"rows":["S1"]}"#).unwrap().unwrap();
        assert!(answer.contains(quote) && !answer.contains("row"));
        assert_eq!(proof["rows"][0]["kind"], "source_excerpt");
        assert_eq!(
            rows.finish(r#"{"rows":["S1R57"]}"#).unwrap_err(),
            "TABLE_ROW_NOT_VISIBLE"
        );
    }
    #[test]
    fn source_panel_quote_is_the_verified_row_and_never_crosses_revision() {
        let (_, proof) = sample().finish(r#"{"rows":["S1R4"]}"#).unwrap().unwrap();
        let mut sources = json!([{"index":1,"doc_id":"doc","source_sha256":"a".repeat(64)},
            {"index":1,"doc_id":"doc","source_sha256":"b".repeat(64)}]);
        quote_sources(&mut sources, &proof);
        assert_eq!(sources[0]["quote"], "|Alpha|m|0.1|");
        assert!(sources[1].get("quote").is_none());
    }
    #[test]
    fn registry_uses_only_whole_delivered_blocks_and_visible_source_indexes() {
        let block = |index| json!({"schema":"les.evidence-block.v1","source_index":index,"text":"|Item|Qty|\n|A|17.25|"});
        let packet = json!({"sections":[{"kind":"evidence","objects":[{"payload":{"schema":"les.evidence-unit.v1","blocks":[block(1),block(2)]}}]}]});
        let rows = Rows::new(&packet, &json!([{"index":1,"doc_id":"visible"}]));
        assert!(rows.data.contains_key(&(1, 2)) && !rows.data.contains_key(&(2, 2)));
        assert_eq!(
            rows.finish(r#"{"rows":["S2R2"]}"#).unwrap_err(),
            "TABLE_ROW_NOT_VISIBLE"
        );
    }
}
