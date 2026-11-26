//! Compare saved quantities. Inference by quantity is only a candidate link.
use super::{reference, source, text, ModelRows, Result, LIMIT};
use regex::Regex;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::LazyLock};

pub(super) struct Position {
    sheet: String,
    row: u64,
    number: String,
    name: String,
    mark: Option<String>,
    code: Option<String>,
    quantity: u64,
}

pub(super) struct Note {
    pub raw: String,
    pub source: Value,
}

static CONTROLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\[A-Za-z][^;\\{}\r\n]*;|\\[Pp]|[{}]").unwrap());
static COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([0-9]{1,9})\s*[-–—]\s*(\S.*)$").unwrap());
static POWER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^\w.,])([0-9]{1,9}(?:[.,][0-9]+)?)\s*(?:вт|w)\b").unwrap()
});

fn clean_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn integer(value: &Value) -> Option<u64> {
    let number = value
        .as_f64()
        .or_else(|| text(value)?.trim().parse().ok())?;
    (number > 0.0 && number <= 1_000_000_000.0 && number.fract() == 0.0).then_some(number as u64)
}

fn code(value: &str) -> &str {
    value
        .strip_suffix(".0")
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(value)
}

fn position(row: &Value) -> Option<Position> {
    let sheet = text(&row["sheet_name"])?;
    let coordinate = row["source_row"].as_u64()?;
    let number = text(&row["pos"]).or_else(|| text(&row["position"]))?;
    let name = text(&row["name"])?;
    if !matches!(
        clean_key(text(&row["unit"])?).as_str(),
        "шт" | "штука" | "pcs" | "pc" | "ea"
    ) {
        return None;
    }
    let quantity = integer(&row["qty"]).or_else(|| {
        // Existing books may preserve headers like «Кол.» only in the original cells.
        let raw: Value = serde_json::from_str(text(&row["raw_row"])?).ok()?;
        let mut values = raw
            .as_object()?
            .iter()
            .filter(|(key, _)| {
                matches!(
                    clean_key(key).as_str(),
                    "кол" | "колво" | "количество" | "quantity" | "qty"
                )
            })
            .filter_map(|(_, value)| integer(value));
        values.next().filter(|_| values.next().is_none())
    })?;
    Some(Position {
        sheet: sheet.into(),
        row: coordinate,
        number: code(number).into(),
        name: name.into(),
        quantity,
        mark: text(&row["mark"]).map(str::to_owned),
        code: text(&row["code"]).map(|v| code(v).to_owned()),
    })
}

pub(super) fn positions(rows: &[Value], args: &Value) -> Result<Vec<Position>> {
    let sheet = text(&args["spec_sheet"]);
    let first = args["row_start"].as_u64();
    let last = args["row_end"].as_u64();
    if first.is_some() != last.is_some()
        || first
            .zip(last)
            .is_some_and(|(a, b)| a == 0 || a > b || sheet.is_none())
    {
        return Err("DWG_SPEC_RANGE_INVALID".into());
    }
    let mut section = None;
    if first.is_none() {
        let mut headings = rows.iter().filter(|row| {
            sheet.is_none_or(|name| row["sheet_name"] == name)
                && text(&row["name"])
                    .is_some_and(|name| clean_key(name) == "светотехническаяпродукция")
                && row["qty"].is_null()
                && text(&row["unit"]).is_none()
        });
        if let Some(heading) = headings.next() {
            if headings.next().is_some() {
                return Err("DWG_SPEC_RANGE_INVALID".into());
            }
            let sheet = text(&heading["sheet_name"]).ok_or("INDEX_READ_INVALID")?;
            let first = heading["source_row"].as_u64().ok_or("INDEX_READ_INVALID")?;
            let end = rows
                .iter()
                .filter(|r| r["sheet_name"] == sheet)
                .filter(|r| text(&r["name"]).is_some() && text(&r["unit"]).is_none())
                .filter(|r| {
                    text(&r["pos"]).is_some_and(|p| {
                        p.bytes().all(|b| matches!(b, b'I' | b'V' | b'X')) && p.len() <= 8
                    })
                })
                .filter_map(|r| r["source_row"].as_u64())
                .filter(|row| *row > first)
                .min();
            section = Some((sheet, first, end.unwrap_or(u64::MAX)));
        }
    }
    let mut result = rows
        .iter()
        .filter_map(position)
        .filter(|p| {
            sheet.is_none_or(|s| p.sheet == s)
                && first
                    .zip(last)
                    .is_none_or(|(a, b)| (a..=b).contains(&p.row))
                && section.is_none_or(|(s, a, b)| p.sheet == s && p.row > a && p.row < b)
        })
        .collect::<Vec<_>>();
    if result.is_empty() {
        return Err("DWG_SPEC_NO_POSITIONS".into());
    }
    if result.len() > 2000 {
        return Err(LIMIT.into());
    }
    result.sort_by(|a, b| (&a.sheet, a.row).cmp(&(&b.sheet, b.row)));
    Ok(result)
}

struct Identity {
    words: Vec<String>,
    emergency: bool,
    temperature: Option<String>,
}

fn identity(label: &str) -> Identity {
    let plain = label.to_lowercase().replace(['×', 'х'], "x");
    let parts: Vec<_> = plain
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let temperature = parts.iter().enumerate().find_map(|(index, word)| {
        let (number, end) = if let Some(number) = word.strip_suffix(['k', 'к']) {
            (number, index + 1)
        } else if matches!(parts.get(index + 1).copied(), Some("k" | "к")) {
            (*word, index + 2)
        } else {
            return None;
        };
        (number.len() == 4 && number.bytes().all(|b| b.is_ascii_digit()))
            .then(|| (index..end, number.to_owned()))
    });
    let emergency = parts
        .iter()
        .any(|v| matches!(*v, "em" | "emergency" | "бап"));
    let words = parts
        .into_iter()
        .enumerate()
        .filter(|(i, word)| {
            word.is_ascii()
                && !matches!(*word, "em" | "emergency")
                && temperature
                    .as_ref()
                    .is_none_or(|(span, _)| !span.contains(i))
        })
        .map(|(_, word)| word.to_owned())
        .collect();
    Identity {
        words,
        emergency,
        temperature: temperature.map(|(_, value)| value),
    }
}

fn score(label: &Identity, product: &Identity) -> usize {
    if label.emergency != product.emergency
        || label
            .temperature
            .as_ref()
            .is_some_and(|a| product.temperature.as_ref().is_some_and(|b| a != b))
    {
        return 0;
    }
    product
        .words
        .iter()
        .enumerate()
        .filter_map(|(i, _)| {
            let equal = label
                .words
                .iter()
                .zip(&product.words[i..])
                .take_while(|(a, b)| a == b)
                .count();
            let shared = &label.words[..equal];
            // Require a physical model identifier, not a short shared product-family word.
            if equal < 2
                || !shared
                    .iter()
                    .any(|word| word.bytes().any(|b| b.is_ascii_digit()))
                || !shared
                    .iter()
                    .any(|word| word.bytes().any(|b| b.is_ascii_alphabetic()))
            {
                return None;
            }
            if let Some(next) = label.words.get(equal).zip(product.words.get(i + equal)) {
                if [next.0, next.1]
                    .iter()
                    .all(|n| n.as_bytes()[0].is_ascii_digit())
                    && next.0.bytes().all(|b| b.is_ascii_digit())
                    && next.1.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
            }
            Some(equal)
        })
        .max()
        .unwrap_or(0)
}

fn unique_best(scores: impl Iterator<Item = usize>) -> Option<usize> {
    let mut winner = None;
    let mut best = 0;
    for (i, score) in scores.enumerate() {
        if score > best {
            best = score;
            winner = Some(i);
        } else if score == best {
            winner = None;
        }
    }
    winner
}

#[derive(Default)]
struct Observations {
    quantity: Option<u64>,
    unquantified: usize,
    details: Vec<Value>,
    examples: Vec<Value>,
}

impl Observations {
    fn record(&mut self, quantity: Option<u64>, evidence: Value) -> Result<()> {
        if let Some(quantity) = quantity {
            self.quantity = Some(
                self.quantity
                    .unwrap_or(0)
                    .checked_add(quantity)
                    .ok_or(LIMIT)?,
            );
        } else {
            self.unquantified += 1;
        }
        self.details.push(json!({"doc_id":evidence["doc_id"],
            "handle":evidence["model_locator"]["native_id"],"quantity":quantity}));
        if self.examples.len() < 2 {
            self.examples.push(evidence);
        }
        Ok(())
    }
}

#[derive(Default)]
struct BlockGroup {
    quantity: u64,
    examples: BTreeMap<String, Value>,
}

fn attr<'a>(row: &'a Value, tags: &[&str]) -> Option<&'a str> {
    let mut values = row["attributes"]
        .as_array()?
        .iter()
        .filter(|item| {
            text(&item["tag"]).is_some_and(|tag| tags.contains(&tag.to_uppercase().as_str()))
        })
        .filter_map(|item| text(&item["value"]));
    let first = values.next()?;
    values.all(|v| v == first).then_some(first)
}

pub(super) fn table(
    book: &Value,
    positions: Vec<Position>,
    models: &[&ModelRows],
    notes: Vec<Note>,
    args: &Value,
) -> Result<(Value, Vec<Value>)> {
    let names: Vec<_> = positions
        .iter()
        .map(|p| [Some(identity(&p.name)), p.mark.as_deref().map(identity)])
        .collect();
    let identify = |name: &str| {
        let label = identity(name);
        unique_best(names.iter().map(|available| {
            available
                .iter()
                .flatten()
                .map(|product| score(&label, product))
                .max()
                .unwrap_or(0)
        }))
    };
    let mut annotations: Vec<Observations> =
        positions.iter().map(|_| Observations::default()).collect();
    let mut blocks: Vec<Observations> = positions.iter().map(|_| Observations::default()).collect();
    let mut groups = BTreeMap::<(&str, &str, &str), BlockGroup>::new();
    let mut ordered_notes = notes;
    ordered_notes.sort_by(|a, b| {
        (
            text(&a.source["doc_name"]),
            text(&a.source["model_locator"]["native_id"]),
        )
            .cmp(&(
                text(&b.source["doc_name"]),
                text(&b.source["model_locator"]["native_id"]),
            ))
    });
    let note_count = ordered_notes.len();
    for note in ordered_notes {
        let plain = CONTROLS.replace_all(&note.raw, " ");
        let single_line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        let count = COUNT.captures(&single_line);
        let label = count
            .as_ref()
            .and_then(|m| m.get(2))
            .map_or(single_line.as_str(), |m| m.as_str());
        if let Some(i) = identify(label) {
            let quantity = count
                .and_then(|m| m[1].parse::<u64>().ok())
                .filter(|n| *n > 0);
            annotations[i].record(quantity, note.source)?;
        }
    }
    for model in models {
        for row in &model.records {
            let saved = &row["payload"]["indexed_engineering_record"];
            let name = text(&saved["name"]).ok_or("INDEX_READ_INVALID")?;
            let quantity = saved["quantity"]
                .as_u64()
                .filter(|q| *q > 0)
                .ok_or("INDEX_READ_INVALID")?;
            let article = attr(
                saved,
                &["ARTICLE", "АРТИКУЛ", "PRODUCT_CODE", "КОД ПРОДУКЦИИ"],
            );
            let definite = if let Some(article) = article {
                unique_best(positions.iter().map(|p| {
                    usize::from(
                        p.code
                            .as_deref()
                            .is_some_and(|v| v.eq_ignore_ascii_case(code(article))),
                    )
                }))
            } else {
                identify(name)
            };
            if let Some(i) = definite {
                blocks[i].record(Some(quantity), source(&model.doc, row))?;
            } else if article.is_none() && !name.starts_with('*') {
                let position = attr(saved, &["№СП", "POS", "POSITION", "ПОЗ", "ПОЗИЦИЯ"]);
                if let (Some(position), Some(layer)) = (position, text(&saved["layer"])) {
                    let group = groups.entry((name, layer, position)).or_default();
                    group.quantity = group.quantity.checked_add(quantity).ok_or(LIMIT)?;
                    let doc = text(&model.doc["file_name"]).ok_or("INDEX_READ_INVALID")?;
                    group
                        .examples
                        .entry(doc.into())
                        .or_insert_with(|| source(&model.doc, row));
                }
            }
        }
    }
    let mut by_quantity = BTreeMap::<u64, usize>::new();
    for p in &positions {
        *by_quantity.entry(p.quantity).or_default() += 1;
    }
    let mut summary = BTreeMap::<&str, usize>::new();
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 24) as usize;
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let mut rows = Vec::new();
    let mut sources = Vec::new();
    for (i, p) in positions.iter().enumerate() {
        let note = &annotations[i];
        let block = &blocks[i];
        let exact_note = note.quantity == Some(p.quantity) && note.unquantified == 0;
        let block_count = block.quantity;
        let likely = if !exact_note
            && block_count.is_none()
            && !note.details.is_empty()
            && p.quantity > 1
            && by_quantity[&p.quantity] == 1
        {
            let mut matches = groups.iter().filter(|(_, g)| g.quantity == p.quantity);
            matches.next().filter(|_| matches.next().is_none())
        } else {
            None
        };
        let status = if block_count == Some(p.quantity)
            && note.quantity.is_none_or(|v| v == p.quantity)
            || exact_note && block_count.is_none()
        {
            "matches"
        } else if likely.is_some() {
            "candidate"
        } else if block_count.is_some() || note.quantity.is_some() {
            "observed_difference"
        } else if note.unquantified > 0 {
            "unquantified"
        } else {
            "unmatched"
        };
        *summary.entry(status).or_default() += 1;
        if !(offset..offset + limit).contains(&i) {
            continue;
        }
        let mut spec = reference(book, Value::Null);
        spec["sheet_name"] = json!(p.sheet);
        spec["row"] = json!(p.row);
        spec["locator"]["sheet"] = json!(p.sheet);
        spec["locator"]["row"] = json!(p.row);
        spec["snippet"] = json!(p.name.chars().take(240).collect::<String>());
        spec["evidence_kind"] = json!("indexed_workbook_row");
        spec["evidence_admissible"] = json!(true);
        let spec_source = add_source(&mut sources, spec);
        let notes_source = note
            .examples
            .iter()
            .cloned()
            .map(|s| add_source(&mut sources, s))
            .collect::<Vec<_>>();
        let blocks_source = block
            .examples
            .iter()
            .cloned()
            .map(|s| add_source(&mut sources, s))
            .collect::<Vec<_>>();
        let candidate = likely.map(|((name,layer,position),group)| {
            let sources = group.examples.values().take(2).cloned()
                .map(|s|add_source(&mut sources,s)).collect::<Vec<_>>();
            json!({"block":name,"layer":layer,"position_attribute":position,"quantity":group.quantity,
                "basis":"unique_quantity_with_named_annotation","source_indices":sources})
        });
        let nominal = POWER
            .captures_iter(&p.name)
            .filter_map(|m| m[1].replace(',', ".").parse::<f64>().ok())
            .collect::<Vec<_>>();
        rows.push(json!({"position":p.number,"name":p.name,"product_code":p.code,"spec_quantity":p.quantity,
            "spec_unit_power_w":if nominal.len()==1 {nominal.first().copied()} else {None},
            "note_quantity":note.quantity,"notes_without_quantity":note.unquantified,
            "note_delta":note.quantity.map(|v|i128::from(v)-i128::from(p.quantity)),
            "block_quantity":block_count,"block_delta":block_count.map(|v|i128::from(v)-i128::from(p.quantity)),
            "status":status,"candidate":candidate,"source_index":spec_source,"note_source_indices":notes_source,
            "block_source_indices":blocks_source,"note_count":note.details.len(),"block_record_count":block.details.len(),
            "notes":note.details.iter().take(100).collect::<Vec<_>>()}));
    }
    let next = offset + rows.len();
    Ok((
        json!({"schema":"les.dwg-spec-comparison.v1","mode":"dwg_spec","rows":rows,
        "spec_doc_id":book["id"],"spec_doc_name":book["file_name"],"source_sha256":book["file_hash"],
        "total_rows":positions.len(),"summary":summary,"offset":offset,"next_offset":(next<positions.len()).then_some(next),
        "dwg_annotations_read":note_count,"coverage":"indexed_model_space","complete_model":false}),
        sources,
    ))
}

fn add_source(sources: &mut Vec<Value>, mut source: Value) -> usize {
    let index = sources.len() + 1;
    source["index"] = json!(index);
    sources.push(source);
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_variant_and_temperature_and_rejects_ties() {
        let normal = identity("Светильник Fixture.Pro 600 4000K, 28 Вт");
        let emergency = identity("Fixture.Pro 600 EM 4000K");
        let warm = identity("Fixture.Pro 600 3000K");
        let label = identity("Fixture.Pro 600 4000К EM");
        assert_eq!(score(&label, &normal), 0);
        assert!(score(&label, &emergency) > 0);
        assert_eq!(score(&identity("Fixture.Pro 600 4000K"), &warm), 0);
        assert_eq!(score(&identity("Fixture.Pro 601"), &normal), 0);
        assert!(
            score(
                &identity("Flood LED 55 5000К крепление под 30 градусами"),
                &identity("Прожектор Flood LED 55 5000 K IP66")
            ) > 0
        );
        assert_eq!(unique_best([0, 4, 4].into_iter()), None);
        assert_eq!(unique_best([0, 4, 3].into_iter()), Some(1));
    }

    #[test]
    fn reads_literal_quantity_and_keeps_legacy_ambiguity() {
        let mut row = json!({"sheet_name":"1","source_row":37,"pos":"4.0",
            "name":"Fixture.Pro 600 4000K","unit":"шт.","raw_row":"{\"Кол.\":588.0}"});
        assert_eq!(position(&row).unwrap().quantity, 588);
        row["raw_row"] = json!("{\"Кол.\":588,\"Количество\":589}");
        assert!(position(&row).is_none());
        let description =
            CONTROLS.replace_all(r"\A1;\pt4750;10-{\C0;Fixture.Pro 600} \S32/_;", " ");
        let count = COUNT.captures(description.trim()).unwrap();
        assert_eq!(&count[1], "10");
        assert!(COUNT.captures("Fixture.Pro 600 4×18W").is_none());
    }
}
