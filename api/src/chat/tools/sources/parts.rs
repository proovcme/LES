//! Explicit navigation to preserved Word parts; a media listing is not factual evidence.
use super::*;
use std::io::Read;
pub(super) async fn read(context: Arc<Context>, turn: &Prepared, args: Value) -> Result<Value> {
    let doc = document(&context, &args).await?;
    scoped_document(turn, &doc)?;
    let owner = context.clone();
    let source = doc.clone();
    let input = args.clone();
    let result = tokio::task::spawn_blocking(move || read_owned(&owner, &source, &input))
        .await
        .map_err(|_| "SOURCE_READ_FAILED")??;
    let mut reply = envelope("read_source", "document_parts", result, vec![]);
    reply["warnings"]=json!(["Рисунки, схемы и вложения сохранены в исходном виде. Пустой OCR не означает отсутствие содержимого. Не делайте выводов о непроверенных деталях изображения."]);
    Ok(reply)
}

fn navigation(entry: &Value) -> Value {
    let mut value = serde_json::Map::new();
    for key in [
        "label",
        "member",
        "kind",
        "sha256",
        "bytes",
        "url",
        "preview_url",
        "coverage",
        "ocr",
    ] {
        if !entry[key].is_null() {
            value.insert(key.into(), entry[key].clone());
        }
    }
    if let Some(views) = entry["views"].as_array() {
        value.insert(
            "views".into(),
            json!(views
                .iter()
                .map(|v| json!({"sha256":v["sha256"],"url":v["url"],"coverage":v["coverage"]}))
                .collect::<Vec<_>>()),
        );
    }
    Value::Object(value)
}
fn read_owned(context: &Context, source: &Value, input: &Value) -> Result<Value> {
    let expected = input["source_sha256"].as_str().unwrap_or("");
    if input["view"] == "parts" {
        let mut manifest =
            crate::documents::parts::manifest(context, source, expected).map_err(str::to_owned)?;
        let rows = manifest["parts"].as_array().ok_or("SOURCE_PART_INVALID")?;
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        if offset > rows.len() {
            return Err("SOURCE_OFFSET_INVALID".into());
        }
        let total = rows.len();
        let end = (offset + input["limit"].as_u64().unwrap_or(12).clamp(1, 32) as usize).min(total);
        let page = rows[offset..end].iter().map(navigation).collect::<Vec<_>>();
        manifest["parts"] = json!(page);
        manifest["total"] = json!(total);
        manifest["next_offset"] = if end < total { json!(end) } else { Value::Null };
        return Ok(manifest);
    }
    if input["view"] != "part" {
        return Err("SOURCE_VIEW_INVALID".into());
    }
    let member = input["member"].as_str();
    let manifest =
        crate::documents::parts::manifest(context, source, expected).map_err(str::to_owned)?;
    let id = part_id(&manifest, input)?;
    let (mut file, _, entry) =
        crate::documents::parts::open_member(context, source, expected, &id, member)
            .map_err(str::to_owned)?;
    let text = if entry["kind"] == "structure" {
        let mut bytes = Vec::new();
        file.by_ref()
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "SOURCE_READ_FAILED")?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("SOURCE_READ_LIMIT".into());
        }
        String::from_utf8(bytes).map_err(|_| "SOURCE_PART_INVALID")?
    } else {
        entry["selected_view"]["text"]
            .as_str()
            .or_else(|| entry["text"].as_str())
            .unwrap_or("")
            .to_owned()
    };
    let offset = input["offset"].as_u64().unwrap_or(0) as usize;
    let total = text.chars().count();
    if offset > total {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let take = input["max_chars"].as_u64().unwrap_or(3000).clamp(1, 16000) as usize;
    let end = (offset + take).min(total);
    let excerpt = text.chars().skip(offset).take(take).collect::<String>();
    Ok(
        json!({"part":navigation(&entry),"part_sha256":id,"source_sha256":source["file_hash"],"text":excerpt,"offset":offset,
        "next_offset":if end<total{Some(end)}else{None},"complete":end==total,
        "evidence_admissible":false,"visual_verification_required":true,
        "note":"Результат относится только к part.member, это точное имя объекта внутри DOCX. Не подменяйте им другой рисунок и не объявляйте разные имена псевдонимами. Оригинал сохранён без изменения. OCR и структура служат навигации; этот результат сам по себе не подтверждает геометрию или все надписи рисунка."}),
    )
}
fn part_id(manifest: &Value, input: &Value) -> Result<String> {
    let member = input["member"].as_str();
    let supplied = input["part_id"].as_str();
    if member.is_none() {
        return supplied
            .map(str::to_owned)
            .ok_or_else(|| "SOURCE_PART_REQUIRED".into());
    }
    let matches: Vec<_> = manifest["parts"]
        .as_array()
        .ok_or("SOURCE_PART_INVALID")?
        .iter()
        .filter(|p| p["member"] == member.unwrap())
        .collect();
    if matches.is_empty() {
        return Err("SOURCE_PART_NOT_FOUND".into());
    }
    let mut ids = matches
        .iter()
        .filter_map(|p| p["sha256"].as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != 1 {
        return Err("SOURCE_PART_IDENTITY_AMBIGUOUS".into());
    }
    if let Some(id) = supplied {
        if !matches.iter().any(|p| {
            p["sha256"] == id
                || p["preview_sha256"] == id
                || p["views"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v["sha256"] == id))
        }) {
            return Err("SOURCE_PART_IDENTITY_MISMATCH".into());
        }
        Ok(id.into())
    } else {
        Ok(ids[0].into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_member_never_uses_neighboring_picture_or_an_ambiguous_name() {
        let manifest = json!({"parts":[
            {"member":"word/media/image6.emf","sha256":"six","text":"Neighbor OCR"},
            {"member":"word/media/image7.emf","sha256":"seven","text":"","views":[{"sha256":"visible-seven"}]}
        ]});
        assert_eq!(
            part_id(&manifest, &json!({"member":"word/media/image7.emf"})).unwrap(),
            "seven"
        );
        assert!(part_id(
            &manifest,
            &json!({"member":"word/media/image7.emf","part_id":"six"})
        )
        .is_err());
        assert!(part_id(&manifest, &json!({"member":"../image7.emf"})).is_err());
        assert_eq!(
            part_id(
                &manifest,
                &json!({"member":"word/media/image7.emf","part_id":"visible-seven"})
            )
            .unwrap(),
            "visible-seven"
        );
        let duplicate =
            json!({"parts":[{"member":"same","sha256":"one"},{"member":"same","sha256":"two"}]});
        assert!(part_id(&duplicate, &json!({"member":"same"})).is_err());
    }
}
