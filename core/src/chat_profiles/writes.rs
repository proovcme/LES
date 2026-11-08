use super::*;
pub(super) fn execute(
    conn: &Connection,
    action: &str,
    args: &Value,
    request: &Value,
) -> Result<Value> {
    match action {
        "publish_text" => publish_text(conn, args),
        "publish_profile" => publish_profile(conn, args, request),
        "activate" => {
            let mode = mode(text(args, "mode")?)?;
            let id = text(args, "revision_id")?;
            let raw=conn.query_row("SELECT mode,snapshot_json FROM les_profile_revisions WHERE revision_id=? AND deleted_at IS NULL",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?.ok_or_else(||invalid("Версия профиля не найдена"))?;
            if raw.0 != mode {
                return Err(invalid("Версия профиля относится другому режиму"));
            }
            conn.execute("INSERT INTO les_active_profiles(mode,profile_revision_id,updated_at) VALUES(?,?,?) ON CONFLICT(mode) DO UPDATE SET profile_revision_id=excluded.profile_revision_id,updated_at=excluded.updated_at",params![mode,id,now()])?;
            serde_json::from_str(&raw.1).map_err(|_| invalid("Snapshot профиля повреждён"))
        }
        "delete" => {
            let kind = text(args, "kind")?;
            let table = table(kind)?;
            let id = text(args, "revision_id")?;
            let factory = conn
                .query_row(
                    &format!(
                        "SELECT is_factory FROM {table} WHERE revision_id=? AND deleted_at IS NULL"
                    ),
                    [id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(|| invalid("Редакция не найдена"))?;
            if factory != 0 {
                return Err(invalid("Заводскую Base-версию удалить нельзя"));
            }
            if kind == "profile"
                && conn
                    .query_row(
                        "SELECT 1 FROM les_active_profiles WHERE profile_revision_id=?",
                        [id],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?
                    .is_some()
            {
                return Err(invalid("Сначала назначьте другую активную версию профиля"));
            }
            let deleted = now();
            conn.execute(
                &format!("UPDATE {table} SET deleted_at=? WHERE revision_id=?"),
                params![deleted, id],
            )?;
            Ok(json!({"status":"deleted","kind":kind,"revision_id":id,"deleted_at":deleted}))
        }
        _ => Err(invalid("Неизвестное действие профиля")),
    }
}
fn publish_text(conn: &Connection, args: &Value) -> Result<Value> {
    let kind = text(args, "kind")?;
    let value = validated(kind, text(args, "text")?)?;
    let name = trim(text(args, "name")?);
    if name.is_empty() {
        return Err(invalid("Название редакции не задано"));
    }
    let table = table(kind)?;
    let source = optional(args, "source_revision_id");
    if let Some(id) = source {
        if conn
            .query_row(
                &format!("SELECT 1 FROM {table} WHERE revision_id=?"),
                [id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_none()
        {
            return Err(invalid("Исходная редакция не найдена"));
        }
    }
    let revision = conn.query_row(&format!("SELECT COUNT(*)+1 FROM {table}"), [], |r| {
        r.get::<_, i64>(0)
    })?;
    let id = format!("user:{kind}:{}", uuid()?);
    let created = now();
    let hash = crate::sha256_hex(value.as_bytes());
    conn.execute(&format!("INSERT INTO {table} (revision_id,name,revision_no,text_value,sha256,is_factory,source_revision_id,created_at) VALUES(?,?,?,?,?,0,?,?)"),params![id,name,revision,value,hash,source,created])?;
    Ok(
        json!({"revision_id":id,"name":name,"revision_no":revision,"text":value,"sha256":hash,
        "is_factory":false,"source_revision_id":source,"created_at":created}),
    )
}
fn publish_profile(conn: &Connection, args: &Value, request: &Value) -> Result<Value> {
    for key in ["model_policy", "rag_policy"] {
        if args
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(invalid(format!("Некорректное поле {key}")));
        }
    }
    if args
        .get("additional_skill_revision_ids")
        .is_some_and(|v| !v.is_null() && !v.is_array())
    {
        return Err(invalid("Некорректный список дополнительных скиллов"));
    }
    let mode = mode(text(args, "mode")?)?;
    let name = trim(text(args, "name")?);
    if name.is_empty() {
        return Err(invalid("Название версии профиля не задано"));
    }
    let model = args["model_policy"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    if model
        .get("reasoning_enabled")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid(
            "reasoning_enabled должен быть логическим значением",
        ));
    }
    if model
        .get("reasoning_budget_tokens")
        .is_some_and(|v| !v.as_i64().is_some_and(|v| (512..=8192).contains(&v)))
    {
        return Err(invalid(
            "Бюджет размышления и ответа должен быть от 512 до 8192 токенов",
        ));
    }
    let mut tools = Vec::new();
    for value in args["tools"]
        .as_array()
        .ok_or_else(|| invalid("Некорректный список инструментов"))?
    {
        let value = value
            .as_str()
            .ok_or_else(|| invalid("Некорректное имя инструмента"))?
            .trim();
        if !value.is_empty() && !tools.iter().any(|v| v == value) {
            tools.push(value.to_owned());
        }
    }
    let known = request
        .get("known_tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| defaults()["agent"]["tools"].as_array().unwrap().clone());
    let known = known
        .iter()
        .filter_map(Value::as_str)
        .collect::<HashSet<_>>();
    let mut unknown = tools
        .iter()
        .filter(|s| !known.contains(s.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(invalid(format!(
            "Неизвестные инструменты: {}",
            unknown.join(", ")
        )));
    }
    let prompt_id = text(args, "prompt_revision_id")?;
    let skill_id = text(args, "skill_revision_id")?;
    let prompt = text_revision(conn, "prompt", prompt_id, true)?;
    let skill = text_revision(conn, "skill", skill_id, true)?;
    let mut extra_ids = Vec::new();
    if let Some(values) = args["additional_skill_revision_ids"].as_array() {
        for id in values {
            let id = id
                .as_str()
                .ok_or_else(|| invalid("Некорректная редакция скилла"))?;
            if id != skill_id && !extra_ids.contains(&id) {
                extra_ids.push(id);
            }
        }
    }
    if extra_ids.len() > 15 {
        return Err(invalid(
            "В профиль можно добавить не более 15 дополнительных скиллов",
        ));
    }
    let extras = extra_ids
        .iter()
        .map(|id| text_revision(conn, "skill", id, true))
        .collect::<Result<Vec<_>>>()?;
    let texts = std::iter::once(skill["text"].as_str().unwrap())
        .chain(extras.iter().map(|v| v["text"].as_str().unwrap()))
        .collect::<Vec<_>>();
    validated("skill", &texts.join("\n\n"))?;
    let source = optional(args, "source_revision_id");
    if let Some(id) = source {
        let old = conn
            .query_row(
                "SELECT mode FROM les_profile_revisions WHERE revision_id=? AND deleted_at IS NULL",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| invalid("Исходная версия профиля не найдена"))?;
        if old != mode {
            return Err(invalid("Исходная версия относится другому режиму"));
        }
    }
    let revision = conn.query_row(
        "SELECT COUNT(*)+1 FROM les_profile_revisions WHERE mode=?",
        [mode],
        |r| r.get::<_, i64>(0),
    )?;
    let id = format!("user:profile:{mode}:{}", uuid()?);
    let created = now();
    let base = json!({"prompt":prompt["text"],"skill":skill["text"],"tools":tools,"model_policy":model,
        "rag_policy":args["rag_policy"].as_object().cloned().unwrap_or_default()});
    let mut value = snapshot(
        &base, &id, mode, name, revision, prompt_id, skill_id, &created, false,
    );
    value["additional_skills"]=json!(extras.iter().map(|v|json!({"revision_id":v["revision_id"],"name":v["name"],"text":v["text"],"sha256":v["sha256"]})).collect::<Vec<_>>());
    conn.execute("INSERT INTO les_profile_revisions(revision_id,mode,name,revision_no,prompt_revision_id,skill_revision_id,tools_json,model_policy_json,rag_policy_json,snapshot_json,is_factory,source_revision_id,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,0,?,?)",
        params![id,mode,name,revision,prompt_id,skill_id,value["tools"].to_string(),value["model_policy"].to_string(),value["rag_policy"].to_string(),value.to_string(),source,created])?;
    Ok(value)
}
