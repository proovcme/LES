//! The model selects visible rows. Native code copies cells and computes sums.
//! This proves source binding, not relevance of the selected row to the question.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
mod addresses;
pub(super) mod lookup;
mod quantities;
use addresses::Reference;
#[derive(Default)]
pub(super) struct Rows {
    data: BTreeMap<(u64, u64), (Vec<String>, Value)>,
    paragraphs: BTreeMap<(u64, u64), (Vec<String>, Value)>,
    headers: BTreeMap<(u64, u64), Vec<String>>,
    literal_sources: HashSet<u64>,
    cell_sources: HashSet<u64>,
}
pub(super) const SUM_CONTRACT: &str = "Для явно запрошенного сложения видимых табличных ячеек можно передать вычисление Лесу: верни один JSON с answerable=true, rows с адресами S…R… всех слагаемых и sums с объектами column (номер C) и unit_column (номер C или null). При обычном вопросе о содержимом таблицы отвечай текстом со ссылками.";
pub(super) const ROW_CONTRACT: &str = "Для ответа выбери строки и выдержки из текущего окна. Верни один JSON с полями answerable (да/нет как true/false), rows (массив видимых адресов S1R2, S1P3 или S1 из соответствующего фрагмента), sums (пустой массив либо запрошенные суммы с column и unit_column) и missing (чего не хватает). Если сведений нет, верни answerable=false и rows=[]. Значения и единицы по выбранным адресам выведет Лес.";

pub(super) fn cell_window(initial: &[Value], required: &[Value]) -> bool {
    let mut seen = false;
    for block in initial.iter().chain(required) {
        if matches!(
            block["schema"].as_str(),
            Some("les.evidence-unit.v1" | "les.evidence-block.v1")
        ) {
            if !cell_block(block) {
                return false;
            }
            seen = true;
        }
    }
    seen
}
fn cell_block(block: &Value) -> bool {
    if block["schema"] == "les.evidence-unit.v1" {
        return block["blocks"]
            .as_array()
            .is_some_and(|items| !items.is_empty() && items.iter().all(cell_block));
    }
    block["verified_cells"] == true
        || block["record_kind"] == "spreadsheet_row"
        || block["canonical"]["kind"] == "table"
}

/// The fixed wire schema fits the budget before the evidence packet is assembled.
/// Actual address membership, numeric columns and units are checked against that packet.
pub(super) fn selection_format() -> Value {
    json!({"type":"json_schema","json_schema":{"name":"les_source_rows","strict":true,"schema":{
    "type":"object","additionalProperties":false,"required":["answerable","rows","sums","missing"],"properties":{
        "answerable":{"type":"boolean"},"rows":{"type":"array","items":{"type":"string"},"maxItems":128},
        "sums":{"type":"array","maxItems":16,"items":{"type":"object","additionalProperties":false,
            "required":["column","unit_column"],"properties":{"column":{"type":"integer","minimum":1},
            "unit_column":{"type":["integer","null"],"minimum":1}}}},
        "missing":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":160}}
    }}}})
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
    #[cfg(test)]
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
        "type":"object","additionalProperties":false,"required":["answerable","rows","sums","missing"],"properties":{
            "answerable":{"type":"boolean"},
            "missing":{"type":"array","maxItems":8,"items":{"type":"string","maxLength":160}},
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
                || b["table_geometry_unverified"] == true
                || b["canonical"]["schema"] == "les.document-structure.v1"
                || !les_core::chat_context::table_rows(b["text"].as_str().unwrap_or("")).is_empty()
        })
    }
    pub(super) fn present(&self) -> bool {
        !self.data.is_empty() || !self.paragraphs.is_empty()
    }
    pub(super) fn needs_selection(
        &self,
        answer: &str,
        sources: &Value,
    ) -> Result<bool, &'static str> {
        if !self.present() {
            return Ok(false);
        }
        let references = les_core::citations::execute(&json!({"answer":answer,"sources":sources}))?;
        let mut excerpts = Vec::new();
        let mut all_pdf = true;
        for index in references["indexes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
        {
            if self.cell_sources.contains(&index) {
                return Ok(true);
            }
            if let Some((cells, source)) = self.data.get(&(index, 0)) {
                if source["doc_name"]
                    .as_str()
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".pdf"))
                {
                    excerpts.extend(cells.iter().map(String::as_str));
                    continue;
                }
            }
            all_pdf = false;
        }
        Ok(all_pdf && !excerpts.is_empty() && quantities::contradicts(answer, &excerpts))
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
            let canonical = &value["canonical"];
            if cell_block(value) {
                out.cell_sources.insert(index);
            }
            if value["table_geometry_unverified"] == true {
                if let Some(body) = value["body_offset"]
                    .as_u64()
                    .and_then(|n| value["text"].as_str()?.get(n as usize..))
                {
                    out.literal_sources.insert(index);
                    out.data
                        .insert((index, 0), (vec![body.to_owned()], source.clone()));
                }
                return;
            }
            if canonical["schema"] == "les.document-structure.v1" {
                out.literal_sources.insert(index);
                if canonical["kind"] == "paragraph" {
                    if let Some(text) = canonical["text"].as_str() {
                        out.paragraphs
                            .insert((index, 1), (vec![text.to_owned()], source.clone()));
                        out.data
                            .insert((index, 0), (vec![text.to_owned()], source.clone()));
                    }
                } else if canonical["kind"] == "table" {
                    let strings = |row: &Value| {
                        row.as_array()?
                            .iter()
                            .map(|c| c.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    };
                    let header = strings(&canonical["header"]);
                    for (r, row) in canonical["rows"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                    {
                        if let Some(cells) = strings(row) {
                            if let Some(header) = &header {
                                out.headers.insert((index, r as u64 + 1), header.clone());
                            }
                            out.data
                                .insert((index, r as u64 + 1), (cells, source.clone()));
                        }
                    }
                }
                return; // Typed cells are authoritative; never parse presentation.
            }
            let original = value["text"].as_str().unwrap_or("");
            let text = value["body_offset"]
                .as_u64()
                .and_then(|offset| original.get(offset as usize..))
                .unwrap_or(original);
            if value["body_offset"].is_u64() {
                for (line, paragraph) in les_core::chat_context::prose_addresses(text, 0) {
                    out.paragraphs
                        .insert((index, line), (vec![paragraph], source.clone()));
                }
                if !text.trim().is_empty() && les_core::chat_context::table_rows(text).is_empty() {
                    out.data
                        .insert((index, 0), (vec![text.to_owned()], source.clone()));
                }
            }
            let headers = les_core::chat_context::table_headers(text);
            for (row, cells) in les_core::chat_context::table_rows(text) {
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
    pub(super) fn finish_sum(&self, answer: &str) -> Result<Option<(String, Value)>, &'static str> {
        let value: Value = match serde_json::from_str(json_answer(answer)) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if !value["rows"].is_array() && !value["sums"].is_array() {
            return Ok(None);
        }
        if !self.present() {
            return Err("TABLE_FACTS_REQUIRED");
        }
        self.finish_value(&value, true)
    }
    pub(super) fn finish_checked(
        &self,
        answer: &str,
        require_decision: bool,
    ) -> Result<Option<(String, Value)>, &'static str> {
        if !self.present() {
            return Ok(None);
        }
        let text = json_answer(answer);
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
        self.finish_value(&value, require_decision)
    }
    fn finish_value(
        &self,
        value: &Value,
        require_decision: bool,
    ) -> Result<Option<(String, Value)>, &'static str> {
        let calculating = value["sums"]
            .as_array()
            .is_some_and(|sums| !sums.is_empty());
        let object = value.as_object().ok_or("TABLE_FACTS_REQUIRED")?;
        if object
            .keys()
            .any(|k| !["answerable", "rows", "sums", "missing"].contains(&k.as_str()))
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
        if decision == Some(true) && selected.is_empty() {
            return Err("TABLE_FACTS_REQUIRED");
        }
        if selected.is_empty() {
            if value
                .get("sums")
                .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
            {
                return Err("TABLE_FACTS_REQUIRED");
            }
            return Ok(Some((
                "В прочитанных фрагментах не удалось подтвердить запрошенные сведения.".into(),
                json!({"schema":"les.table-facts.v1","status":"insufficient","rows":[],"sums":[]}),
            )));
        }
        let mut rendered = String::new();
        let mut receipt = Vec::new();
        let mut previous_header = Vec::new();
        for id in &selected {
            let key = id.key();
            let (cells, source) = self.get(*id).ok_or("TABLE_ROW_NOT_VISIBLE")?;
            if calculating {
                if !matches!(id, Reference::Row(..)) {
                    return Err("TABLE_VALUE_INVALID");
                }
                receipt.push(
                    json!({"source_index":key.0,"row":key.1,"kind":"table_row","cells":cells,
                    "doc_id":source["doc_id"],"source_sha256":source["source_sha256"]}),
                );
                continue;
            }
            if matches!(id, Reference::Paragraph(..)) {
                if !rendered.is_empty() {
                    rendered.push('\n');
                }
                let text = if self.literal_sources.contains(&key.0) {
                    les_core::chat_context::literal_markdown(&cells[0])
                } else {
                    cells[0].clone()
                };
                rendered.push_str(&format!("{}\n\n[Источник {}]\n", text, key.0));
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
            let escape = |v: &str| {
                if self.literal_sources.contains(&key.0) {
                    les_core::chat_context::literal_markdown(v)
                } else {
                    v.replace('|', "\\|").replace('\n', "<br>")
                }
            };
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
        if calculating {
            let indexes: BTreeSet<_> = selected.iter().map(|id| id.key().0).collect();
            for index in indexes {
                rendered.push_str(&format!(" [Источник {index}]"));
            }
        }
        if selected.iter().any(|id| {
            self.get(*id)
                .is_some_and(|(_, s)| s["table_projection_warning"].is_string())
        }) {
            rendered.push_str("\n\n");
            rendered.push_str(crate::document_parser::TABLE_PROJECTION_NOTICE);
        }
        let missing: Vec<_> = match value.get("missing") {
            Some(items) => items
                .as_array()
                .filter(|items| items.len() <= 8)
                .ok_or("TABLE_FACTS_REQUIRED")?
                .iter()
                .map(|item| {
                    let text = item
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 160)
                        .ok_or("TABLE_FACTS_REQUIRED")?;
                    // A model-authored label is never a citation or a new factual answer.
                    let text = text
                        .replace(['[', ']'], "")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    Ok(les_core::chat_context::literal_markdown(&text))
                })
                .collect::<Result<Vec<_>, &str>>()?,
            None => Vec::new(),
        };
        if decision == Some(false) && missing.is_empty() {
            return Err("TABLE_FACTS_REQUIRED");
        }
        if !missing.is_empty() {
            rendered.push_str("\n\nВ прочитанных источниках не подтверждено — ");
            rendered.push_str(&missing.join(", "));
            rendered.push('.');
        }
        if rendered.chars().count() > 16000 {
            return Err("TABLE_ANSWER_TOO_LARGE");
        }
        Ok(Some((
            rendered.trim().to_owned(),
            json!({"schema":"les.table-facts.v1","status":if missing.is_empty(){"copied"}else{"partial"},"rows":receipt,"sums":sums,"missing":missing}),
        )))
    }
}
fn json_answer(answer: &str) -> &str {
    let answer = answer.trim();
    answer
        .strip_prefix("```json")
        .or_else(|| answer.strip_prefix("```"))
        .map(|text| text.trim().trim_end_matches("```").trim())
        .unwrap_or(answer)
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
    fn unverified_pdf_layout_is_an_exact_excerpt_never_cells_for_arithmetic() {
        let text = "|Valve|117.25|";
        let block = json!({"schema":"les.evidence-block.v1","source_index":1,
            "body_offset":0,"text":text,"table_geometry_unverified":true});
        let source = json!({"index":1,"doc_id":"doc","source_sha256":"revision",
            "table_projection_warning":"PDF_TABLE_SOURCE_MISMATCH"});
        let objects = les_core::chat_context::execute(
            &json!({"action":"objects","prefix":"e","values":[block.clone()]}),
        )
        .unwrap();
        let render = objects[0]["render"].as_str().unwrap();
        assert!(render.contains("[Фрагмент S1]"));
        assert!(!render.contains("S1R") && !render.contains("C2:"));
        assert!(Rows::supported(&[block.clone()]));
        let rows = Rows::new(
            &json!({"sections":[{"kind":"evidence","objects":[{"payload":block}]}]}),
            &json!([source]),
        );
        assert_eq!(
            rows.finish_checked(r#"{"answerable":true,"rows":["S1R1"],"sums":[]}"#, true)
                .unwrap_err(),
            "TABLE_ROW_NOT_VISIBLE"
        );
        let (answer, receipt) = rows
            .finish_checked(r#"{"answerable":true,"rows":["S1"],"sums":[]}"#, true)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["rows"][0]["cells"], json!([text]));
        assert!(answer.contains(crate::document_parser::TABLE_PROJECTION_NOTICE));
        assert_eq!(
            rows.finish_checked(
                r#"{"answerable":true,"rows":["S1"],"sums":[{"column":1,"unit_column":null}]}"#,
                true
            )
            .unwrap_err(),
            "TABLE_VALUE_INVALID"
        );
    }
    #[test]
    fn canonical_cells_drive_model_addresses_and_values_without_markdown_reparsing() {
        let prefix = "[Источник 1]\n";
        let block = json!({"schema":"les.evidence-block.v1","source_index":1,"body_offset":prefix.len(),
            "text":format!("{prefix}|Presentation|117.25|"),
            "canonical":{"schema":"les.document-structure.v1","kind":"table","header":["Item","Quantity"],"rows":[["125*480 | DN_100","17.25"]]}});
        let object = les_core::chat_context::execute(
            &json!({"action":"objects","prefix":"e","values":[block.clone()]}),
        )
        .unwrap();
        let render = object[0]["render"].as_str().unwrap();
        assert!(render.contains("S1R1 C1: 125*480 | DN_100; C2: 17.25"));
        assert!(!render.contains("117.25"));
        let rows = Rows::new(
            &json!({"sections":[{"kind":"evidence","objects":[{"payload":block}]}]}),
            &json!([{"index":1,"doc_id":"doc","source_sha256":"r"}]),
        );
        let (answer, receipt) = rows
            .finish_checked(r#"{"answerable":true,"rows":["S1R1"],"sums":[]}"#, true)
            .unwrap()
            .unwrap();
        assert!(answer.contains("125\\*480 &#124; DN\\_100"));
        assert_eq!(
            receipt["rows"][0]["cells"],
            json!(["125*480 | DN_100", "17.25"])
        );
        assert!(rows
            .finish_checked(r#"{"answerable":true,"rows":["S1R2"],"sums":[]}"#, true)
            .is_err());
    }
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
        let terminal = json!({"sections":[{"kind":"evidence","objects":[{"payload":{
            "schema":"les.evidence-block.v1","source_index":1,"body_offset":0,"text":body
        }}]}]});
        assert_eq!(
            rows.format(true),
            Rows::new(&terminal, &sources).format(true)
        );
        let ids =
            &rows.format(true)["json_schema"]["schema"]["properties"]["rows"]["items"]["enum"];
        for id in ["S1P1", "S1R5", "S1P7"] {
            assert!(ids.as_array().unwrap().contains(&json!(id)));
        }
        let (text, proof) = rows
            .finish_checked(r#"{"answerable":true,"rows":["S1P1"],"sums":[]}"#, true)
            .unwrap()
            .unwrap();
        assert_eq!(text, "Before: 17.35 kW.\n\n[Источник 1]");
        assert_eq!(proof["rows"][0]["paragraph_line"], 1);
        assert_eq!(proof["rows"][0]["kind"], "source_excerpt");
        assert_eq!(proof["rows"][0]["cells"], json!(["Before: 17.35 kW."]));
        assert!(!text.contains("Pump") && !text.contains("29") && !text.contains("42.75"));
        for id in ["S1P5", "S1R1", "S2P1"] {
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
            rows.finish(r#"{"rows":["S1P1"],"sums":[{"column":1,"unit_column":null}]}"#)
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
        assert!(
            !answer.contains("Alpha") && !answer.contains("0.1") && !answer.contains("[Источник")
        );
        let (answer, partial) = rows
            .finish_checked(
                r#"{"answerable":false,"rows":["S1R4"],"sums":[],"missing":["мощность"]}"#,
                true,
            )
            .unwrap()
            .unwrap();
        assert_eq!(partial["status"], "partial");
        assert_eq!(partial["missing"], json!(["мощность"]));
        assert!(answer.contains("[Источник 1]") && answer.contains("мощность"));
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
            literal_sources: HashSet::new(),
            cell_sources: HashSet::new(),
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
        assert!(limited.contains("В прочитанных фрагментах") && !limited.contains("[Источник"));
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
