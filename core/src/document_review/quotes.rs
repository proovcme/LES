//! Model-selected excerpts. Source characters are copied from the current delivered read.
use super::*;
pub(super) fn table(
    fp: &Value,
    id: &Value,
    offset: usize,
    stop: usize,
    chars: &[char],
) -> Result<Value> {
    let read_id = format!("r{}", &digest(&json!([fp, id, offset, stop]))?[..24]);
    let spans=chars.chunks(160).enumerate().map(|(index,part)|{
        let start=offset+index*160;
        json!({"span_id":format!("q{}",index+1),"offset":start,"stop":start+part.len(),"text":part.iter().collect::<String>()})
    }).collect::<Vec<_>>();
    Ok(json!({"read_id":read_id,"quote_spans":spans}))
}
pub(super) fn resolve(read: &Value, snap: &Value, items: &Value) -> Result<(Value, Value)> {
    let Some(items) = items.as_array() else {
        return Ok((items.clone(), json!([])));
    };
    if items.is_empty() || items.len() > 4 {
        return Err(Failure::invalid("Нужны от 1 до 4 цитат"));
    }
    let mut evidence = Vec::new();
    let mut references = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if item.get("span_id").is_none() {
            evidence.push(item.clone());
            continue;
        }
        if item
            .as_object()
            .is_none_or(|v| v.len() != 2 || !v.contains_key("read_id"))
            || item["read_id"].as_str().is_none()
            || item["read_id"] != read["read_id"]
            || read["source_fingerprint"] != snap["fingerprint"]
            || read["selector"] != snap["selector"]
        {
            return Err(Failure::new("REVIEW_EXCERPT_STALE","Выбранная цитата относится к другой порции или редакции. Прочитайте текущую порцию и выберите её read_id и span_id."));
        }
        let span = read["quote_spans"]
            .as_array()
            .and_then(|v| v.iter().find(|s| s["span_id"] == item["span_id"]))
            .ok_or_else(|| {
                Failure::new(
                    "REVIEW_EXCERPT_STALE",
                    "Нет такого span_id в текущей порции чтения",
                )
            })?;
        let content = snap["fragments"]
            .as_array()
            .and_then(|v| v.iter().find(|f| f["id"] == read["fragment_id"]))
            .and_then(|f| f["text"].as_str())
            .ok_or_else(|| Failure::invalid("Неизвестный фрагмент цитаты"))?
            .chars()
            .collect::<Vec<_>>();
        let start = span["offset"]
            .as_u64()
            .ok_or_else(|| Failure::invalid("Некорректный диапазон цитаты"))?
            as usize;
        let stop = span["stop"]
            .as_u64()
            .ok_or_else(|| Failure::invalid("Некорректный диапазон цитаты"))?
            as usize;
        if start >= stop
            || stop > content.len()
            || span["text"] != content[start..stop].iter().collect::<String>()
        {
            return Err(Failure::invalid(
                "Цитата не соответствует исходному диапазону",
            ));
        }
        evidence.push(json!({"fragment_id":read["fragment_id"],"quote":span["text"]}));
        references.push(json!({"evidence_index":index,"read_id":item["read_id"],"span_id":item["span_id"],"fragment_id":read["fragment_id"],"offset":start,"stop":stop,"kind":"source_excerpt_selected_by_model"}));
    }
    Ok((json!(evidence), json!(references)))
}
