use super::*;
fn snapshot_unit(conn: &Connection, r: &Value) -> Result<Value> {
    let task = task(conn, text(r, "owner")?, text(r, "task_id")?)?;
    let unit = task["units"]
        .as_array()
        .and_then(|u| u.iter().find(|u| u["unit_id"] == r["unit_id"]))
        .cloned()
        .ok_or_else(|| Failure::invalid("Раздел не входит в список проверки"))?;
    if unit["selector"] != r["snapshot"]["selector"] {
        return Err(Failure::invalid(
            "Источник не принадлежит выбранному разделу",
        ));
    }
    Ok(task)
}
pub(super) fn remember(conn: &Connection, r: &Value) -> Result<Value> {
    snapshot_unit(conn, r).map_err(|_| Failure::invalid("Раздел не входит в список проверки"))?;
    let snap = &r["snapshot"];
    let fragment = text(r, "fragment")?;
    let text = snap["fragments"]
        .as_array()
        .and_then(|v| v.iter().find(|f| f["id"] == fragment))
        .and_then(|f| f["text"].as_str())
        .ok_or_else(|| Failure::invalid("Неизвестный фрагмент чтения"))?;
    let start = number(r, "start", -1)?;
    let stop = number(r, "stop", -1)?;
    if start < 0 || stop < start || stop as usize > text.chars().count() {
        return Err(Failure::invalid("Некорректный диапазон чтения"));
    }
    conn.execute(
        "INSERT OR IGNORE INTO review_reads VALUES(?,?,?,?,?,?)",
        params![
            text_arg(r, "task_id")?,
            text_arg(r, "unit_id")?,
            text_arg(snap, "fingerprint")?,
            fragment,
            start,
            stop
        ],
    )?;
    Ok(Value::Null)
}
fn text_arg<'a>(r: &'a Value, k: &str) -> Result<&'a str> {
    super::text(r, k)
}
pub(super) fn request_hash(r: &Value) -> Result<String> {
    digest(&json!([
        number(r, "expected_revision", -1)?,
        r["unit_id"],
        r["source_fingerprint"],
        r["status"],
        r["note"],
        r.get("original_evidence").unwrap_or(&r["evidence"]),
        r["dependencies"]
    ]))
}
fn receipt(conn: &Connection, r: &Value, hash: &str) -> Result<Option<Value>> {
    let saved = conn
        .query_row(
            "SELECT digest,revision FROM review_receipts WHERE task_id=? AND request_id=?",
            params![text(r, "task_id")?, text(r, "request_id")?],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    if let Some((previous, revision)) = saved {
        if previous != hash {
            return Err(Failure::invalid(
                "request_id уже использован для другого результата",
            ));
        }
        return Ok(Some(json!(revision)));
    }
    Ok(None)
}
pub(super) fn replayed(conn: &Connection, r: &Value) -> Result<Option<Value>> {
    task(conn, text(r, "owner")?, text(r, "task_id")?)?;
    receipt(conn, r, &request_hash(r)?)
}
pub(super) fn record(conn: &Connection, r: &Value) -> Result<Value> {
    let request = text(r, "request_id")?;
    let expected = number(r, "expected_revision", -1)?;
    if request.is_empty() || request.chars().count() > 120 || expected < 0 {
        return Err(Failure::invalid(
            "Нужны request_id и текущая ревизия проверки",
        ));
    }
    let status = text(r, "status")?;
    let note = text(r, "note")?;
    if !["reviewed", "needs_review", "excluded"].contains(&status)
        || note.trim().is_empty()
        || note.chars().count() > 1200
    {
        return Err(Failure::invalid(
            "Нужны статус и объяснение до 1200 символов",
        ));
    }
    let dependencies = r["dependencies"]
        .as_array()
        .filter(|v| v.len() <= 16 && v.iter().all(Value::is_string))
        .ok_or_else(|| Failure::invalid("Некорректные зависимости вывода"))?;
    if dependencies
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect::<BTreeSet<_>>()
        .len()
        != dependencies.len()
    {
        return Err(Failure::invalid("Некорректные зависимости вывода"));
    }
    let evidence = r["evidence"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 4)
        .ok_or_else(|| Failure::invalid("Нужны от 1 до 4 точных цитат прочитанного раздела"))?;
    let owner = text(r, "owner")?;
    let id = text(r, "task_id")?;
    let unit = text(r, "unit_id")?;
    let fp = text(r, "source_fingerprint")?;
    let hash = request_hash(r)?;
    let task = task(conn, owner, id)?;
    if let Some(revision) = receipt(conn, r, &hash)? {
        return Ok(revision);
    }
    if task["revision"] != expected {
        return Err(Failure::invalid(
            "Ревизия проверки изменилась; прочитайте status",
        ));
    }
    let units = task["units"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Повреждён список проверки"))?;
    if !units.iter().any(|u| u["unit_id"] == unit)
        || dependencies
            .iter()
            .any(|d| d == unit || !units.iter().any(|u| u["unit_id"] == *d))
    {
        return Err(Failure::invalid(
            "Неизвестный раздел или зависимость от самого себя",
        ));
    }
    let snap = &r["snapshot"];
    snapshot_unit(conn, r)?;
    if snap["fingerprint"] != fp {
        return Err(Failure::invalid(
            "Источник изменился после чтения; дочитайте текущую версию",
        ));
    }
    let mut spans = HashMap::<String, Vec<(usize, usize)>>::new();
    let mut stmt=conn.prepare("SELECT fragment,start,stop FROM review_reads WHERE task_id=? AND unit_id=? AND fingerprint=? ORDER BY start")?;
    for row in stmt.query_map(params![id, unit, fp], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (f, start, stop) = row?;
        if start < 0 || stop < start {
            return Err(Failure::invalid("Повреждён диапазон чтения"));
        }
        spans
            .entry(f)
            .or_default()
            .push((start as usize, stop as usize));
    }
    let fragments = snap["fragments"]
        .as_array()
        .ok_or_else(|| Failure::invalid("Некорректный снимок раздела"))?;
    for ranges in spans.values_mut() {
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for &(start, stop) in ranges.iter() {
            if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
                last.1 = last.1.max(stop);
            } else {
                merged.push((start, stop));
            }
        }
        *ranges = merged;
    }
    for reference in r["evidence_references"].as_array().into_iter().flatten() {
        let index = reference["evidence_index"]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .ok_or_else(|| Failure::invalid("Некорректная ссылка цитаты"))?;
        let fragment = text(reference, "fragment_id")?;
        let start = reference["offset"]
            .as_u64()
            .ok_or_else(|| Failure::invalid("Некорректный диапазон цитаты"))?;
        let stop = reference["stop"]
            .as_u64()
            .ok_or_else(|| Failure::invalid("Некорректный диапазон цитаты"))?;
        if index >= evidence.len()
            || evidence[index]["fragment_id"] != fragment
            || start >= stop
            || !spans.get(fragment).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|(a, b)| *a as u64 <= start && *b as u64 >= stop)
            })
        {
            return Err(Failure::new(
                "REVIEW_QUOTE_NOT_READ",
                "Выбранный диапазон цитаты не находится в прочитанной моделью части раздела",
            ));
        }
    }
    for item in evidence {
        if item.as_object().is_none_or(|o| {
            o.len() != 2 || !o.contains_key("fragment_id") || !o.contains_key("quote")
        }) || item["fragment_id"].as_str().is_none()
            || item["quote"]
                .as_str()
                .is_none_or(|q| q.is_empty() || q.chars().count() > 300)
        {
            return Err(Failure::invalid(
                "Укажите fragment_id и точную цитату до 300 символов",
            ));
        }
        let fragment = text(item, "fragment_id")?;
        let quote = text(item, "quote")?;
        let content = fragments
            .iter()
            .find(|f| f["id"] == fragment)
            .and_then(|f| f["text"].as_str())
            .unwrap_or("")
            .chars()
            .collect::<Vec<_>>();
        let found = spans.get(fragment).is_some_and(|spans| {
            spans.iter().any(|(a, b)| {
                *a <= content.len()
                    && content[*a..(*b).min(content.len())]
                        .iter()
                        .collect::<String>()
                        .contains(quote)
            })
        });
        if !found {
            return Err(Failure::new("REVIEW_QUOTE_NOT_READ",
                "Цитата отсутствует в прочитанной части раздела. Скопируйте короткую непрерывную цитату из current_read.text, сохранив символы и пробелы; пересказ и текст другого источника не подходят."));
        }
    }
    if status == "reviewed" {
        for fragment in fragments {
            let mut end = 0;
            for (a, b) in spans.get(text(fragment, "id")?).into_iter().flatten() {
                if *a > end {
                    break;
                }
                end = end.max(*b);
            }
            if end < text(fragment, "text")?.chars().count() {
                return Err(Failure::invalid(
                    "Раздел прочитан не полностью; продолжите read либо выберите needs_review",
                ));
            }
        }
    }
    let events = latest(conn, id)?;
    let fingerprints = r.get("fingerprints");
    if dependencies.iter().any(|d| {
        events
            .get(d.as_str().unwrap())
            .is_none_or(|e| e["observation"]["status"] != "reviewed")
    }) {
        return Err(Failure::invalid("Зависимый вывод ещё не проверен"));
    }
    if let Some(fp) = fingerprints {
        let state = super::status(conn, owner, id, fp)?;
        if dependencies.iter().any(|d| {
            !state["units"]
                .as_array()
                .unwrap()
                .iter()
                .any(|u| u["unit_id"] == *d && u["state"] == "reviewed")
        }) {
            return Err(Failure::invalid(
                "Зависимые выводы отсутствуют или нуждаются в перепроверке",
            ));
        }
    }
    let depmap = dependencies
        .iter()
        .map(|d| {
            (
                d.as_str().unwrap().to_owned(),
                events[d.as_str().unwrap()]["revision"].clone(),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let provenance=evidence.iter().map(|e|json!({"fragment_id":e["fragment_id"],"locator":fragments.iter().find(|f|f["id"]==e["fragment_id"]).unwrap()["locator"]})).collect::<Vec<_>>();
    let observation = json!({"status":status,"note":note,"evidence":evidence,"evidence_references":r.get("evidence_references").cloned().unwrap_or(json!([])),"dependencies":depmap,"source":{"selector":snap["selector"],"sha256":snap["source_sha256"]},"evidence_provenance":provenance});
    let revision = expected
        .checked_add(1)
        .ok_or_else(|| Failure::invalid("Ревизия проверки превышает предел"))?;
    conn.execute(
        "INSERT INTO review_events VALUES(?,?,?,?,?)",
        params![id, unit, revision, fp, encode(&observation)?],
    )?;
    conn.execute(
        "UPDATE review_tasks SET revision=? WHERE id=?",
        params![revision, id],
    )?;
    conn.execute(
        "INSERT INTO review_receipts VALUES(?,?,?,?)",
        params![id, request, hash, revision],
    )?;
    Ok(json!(revision))
}
