use super::*;
pub(super) fn original(r: &Value) -> Result<Value> {
    let doc = &r["document"];
    if doc["status"] != "INDEXED" || doc["file_hash"].as_str().is_none_or(|s| s.is_empty()) {
        return Err(Failure::new(
            "REVIEW_SOURCE_NOT_INDEXED",
            "Источник ещё не проиндексирован или его версия неизвестна",
        ));
    }
    let path = crate::document_source::resolve(
        doc["source_path"].as_str().unwrap_or(""),
        text(doc, "dataset_id")?,
        text(doc, "file_name")?,
        Path::new(text(r, "content_root")?),
    )
    .map_err(|_| {
        Failure::new(
            "REVIEW_ORIGINAL_UNAVAILABLE",
            "Оригинал недоступен; проверьте файл и права чтения",
        )
    })?;
    let mut file = std::fs::File::open(path).map_err(|_| {
        Failure::new(
            "REVIEW_ORIGINAL_UNAVAILABLE",
            "Оригинал недоступен; проверьте файл и права чтения",
        )
    })?;
    let hash = crate::pdf_source::fingerprint(&mut file).map_err(|_| {
        Failure::new(
            "REVIEW_ORIGINAL_UNAVAILABLE",
            "Оригинал недоступен; проверьте файл и права чтения",
        )
    })?;
    if doc["file_hash"] != hash {
        return Err(Failure::new(
            "REVIEW_ORIGINAL_CHANGED",
            "Файл изменился; обновите индекс перед проверкой",
        ));
    }
    Ok(json!({"sha256":hash,"document_id":doc["id"]}))
}
pub(super) fn selector(value: &Value, allowed: &Value, files: &Value) -> Result<Value> {
    let object=value.as_object().ok_or_else(||Failure::invalid("Выберите документ из текущих наборов: dataset_id, file_name и необязательный parent_id"))?;
    if object
        .keys()
        .any(|k| !["dataset_id", "file_name", "parent_id"].contains(&k.as_str()))
        || object.values().any(|v| {
            v.as_str()
                .is_none_or(|s| s.is_empty() || s.chars().count() > 1000)
        })
        || !object.contains_key("dataset_id")
        || !object.contains_key("file_name")
        || !allowed
            .as_array()
            .is_some_and(|a| a.contains(&value["dataset_id"]))
    {
        return Err(Failure::invalid("Выберите документ из текущих наборов: dataset_id, file_name и необязательный parent_id"));
    }
    let name = text(value, "file_name")?;
    let dataset = text(value, "dataset_id")?;
    if name.starts_with(['/', '\\'])
        || name.contains(':')
        || name.split(['/', '\\']).any(|p| p == "..")
        || dataset == "."
        || dataset == ".."
        || dataset.contains(['/', '\\', ':'])
    {
        return Err(Failure::invalid(
            "Нужен относительный путь документа из каталога",
        ));
    }
    if files
        .as_array()
        .is_some_and(|f| !f.is_empty() && !f.contains(&value["file_name"]))
    {
        return Err(Failure::invalid(
            "Документ не входит в выбранную область файлов",
        ));
    }
    Ok(value.clone())
}
pub(super) fn snapshot(r: &Value) -> Result<Value> {
    let selector = selector(&r["selector"], &r["allowed"], &r["files"])?;
    let points = r["points"]
        .as_array()
        .filter(|p| p.len() <= 2048)
        .ok_or_else(|| Failure::invalid("Раздел слишком большой; выберите отдельный parent_id"))?;
    let mut fragments = Vec::new();
    let mut chars = 0;
    for point in points {
        let p = &point["payload"];
        if selector
            .as_object()
            .unwrap()
            .iter()
            .any(|(k, v)| p[k] != *v)
            || p["node_role"] == "navigation"
            || p["evidence_admissible"] == false
        {
            return Err(Failure::invalid(
                "Фрагмент не принадлежит выбранному разделу",
            ));
        }
        let content = p["text"].as_str().unwrap_or("");
        chars += content.chars().count();
        if chars > 2_000_000 {
            return Err(Failure::invalid(
                "Раздел слишком большой; выберите отдельный parent_id",
            ));
        }
        if content.is_empty() {
            continue;
        }
        let id = point["id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| point["id"].as_u64().map(|v| v.to_string()))
            .ok_or_else(|| Failure::invalid("Некорректный ID фрагмента"))?;
        let mut locator = serde_json::Map::new();
        for key in [
            "page",
            "source_page",
            "source_ref",
            "chunk_ord",
            "parent_id",
            "doc_id",
        ] {
            if !p[key].is_null() {
                locator.insert(key.into(), p[key].clone());
            }
        }
        fragments.push(json!({"id":id,"text":content,"locator":locator}));
    }
    let ordinal = |v: &Value| {
        v["locator"]["chunk_ord"]
            .as_i64()
            .or_else(|| v["locator"]["chunk_ord"].as_str()?.parse().ok())
            .unwrap_or(0)
    };
    fragments.sort_by(|a, b| {
        ordinal(a)
            .cmp(&ordinal(b))
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    if fragments.is_empty()
        || fragments
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
            != fragments.len()
    {
        return Err(Failure::invalid(
            "В выбранном разделе нет однозначных индексированных фрагментов",
        ));
    }
    Ok(
        json!({"selector":selector,"source_sha256":r["source"]["sha256"],"fingerprint":digest(&json!([selector,r["source"],fragments]))?,"fragments":fragments,"coverage_scope":"indexed_text_only","original_visual_review":false}),
    )
}
pub(super) fn fragment(snap: &Value, r: &Value) -> Result<Value> {
    let index = number(r, "fragment_index", 0)?;
    let offset = number(r, "offset", 0)?;
    let max = number(r, "max_chars", 4000)?;
    let fragments = snap["fragments"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Некорректный снимок раздела"))?;
    if index < 0 || index as usize >= fragments.len() || offset < 0 || !(200..=4000).contains(&max)
    {
        return Err(Failure::invalid("Некорректная позиция чтения раздела"));
    }
    let fragment = &fragments[index as usize];
    let chars = text(fragment, "text")?.chars().collect::<Vec<_>>();
    if offset as usize >= chars.len() {
        return Err(Failure::invalid("Смещение за пределами фрагмента"));
    }
    let stop = chars.len().min(offset as usize + max as usize);
    let cursor = if stop < chars.len() {
        json!({"fragment_index":index,"offset":stop})
    } else if (index as usize + 1) < fragments.len() {
        json!({"fragment_index":index+1,"offset":0})
    } else {
        Value::Null
    };
    let mut result = json!({"fragment_id":fragment["id"],"locator":fragment["locator"],"offset":offset,"stop":stop,"text":chars[offset as usize..stop].iter().collect::<String>(),"next_cursor":cursor,"source_fingerprint":snap["fingerprint"],"source_sha256":snap["source_sha256"],"selector":snap["selector"],"fragment_count":fragments.len(),"coverage_scope":snap["coverage_scope"],"original_visual_review":false});
    let quotes = super::quotes::table(
        &snap["fingerprint"],
        &fragment["id"],
        offset as usize,
        stop,
        &chars[offset as usize..stop],
    )?;
    result
        .as_object_mut()
        .unwrap()
        .extend(quotes.as_object().unwrap().clone());
    Ok(result)
}
