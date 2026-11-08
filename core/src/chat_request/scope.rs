use super::*;
fn ids(value: &Value) -> Result<Vec<String>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or("CHAT_SCOPE_INVALID")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or("CHAT_SCOPE_INVALID"))
        .collect()
}
fn projects(value: &Value) -> Result<Vec<i64>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or("CHAT_SCOPE_INVALID")?
        .iter()
        .map(|v| {
            v.as_i64()
                .or_else(|| v.as_str()?.trim().parse().ok())
                .filter(|v| *v > 0)
                .ok_or("CHAT_SCOPE_INVALID")
        })
        .collect()
}
pub(super) fn resolve(request: &Value, datasets: &[Value], links: &[Value]) -> Result<Value> {
    let explicit = request["scope"]["scope_type"]
        .as_str()
        .filter(|v| !v.is_empty());
    let legacy = ids(&request["dataset_ids"])?;
    let mut warnings = Vec::new();
    let (kind, project_ids, mut selected, source) = if let Some(kind) = explicit {
        let kind = kind.trim().to_ascii_lowercase();
        let kind = if [
            "none", "all", "project", "projects", "dataset", "datasets", "mixed",
        ]
        .contains(&kind.as_str())
        {
            kind
        } else {
            warnings.push(json!("unknown_scope_type"));
            "none".into()
        };
        (
            kind,
            projects(&request["scope"]["project_ids"])?,
            ids(&request["scope"]["dataset_ids"])?,
            "ui_scope",
        )
    } else if !legacy.is_empty() {
        (
            "datasets".into(),
            vec![],
            legacy.clone(),
            "legacy_dataset_ids",
        )
    } else if let Some(id) = request["project_id"].as_i64().filter(|id| *id > 0) {
        ("project".into(), vec![id], vec![], "legacy_project_id")
    } else if request["dataset_filter"]
        .as_str()
        .is_some_and(|v| !v.trim().is_empty())
    {
        let filter = request["dataset_filter"].as_str().unwrap().trim();
        if filter.eq_ignore_ascii_case("all") {
            ("all".into(), vec![], vec![], "legacy_dataset_filter")
        } else {
            let matched = datasets
                .iter()
                .filter(|d| d["name"] == filter || d["id"] == filter)
                .filter_map(|d| d["id"].as_str().map(str::to_owned))
                .collect();
            ("datasets".into(), vec![], matched, "legacy_dataset_filter")
        }
    } else {
        ("none".into(), vec![], vec![], "default_none")
    };
    if kind == "all" {
        selected = datasets
            .iter()
            .filter_map(|d| d["id"].as_str().map(str::to_owned))
            .collect();
    }
    if ["project", "projects", "mixed"].contains(&kind.as_str()) {
        for id in &project_ids {
            if let Some(link) = links.iter().find(|v| v["id"].as_i64() == Some(*id)) {
                for dataset in ids(&link["dataset_ids"])? {
                    if !selected.contains(&dataset) {
                        selected.push(dataset);
                    }
                }
            } else {
                warnings.push(json!("project_not_found"));
            }
        }
    }
    // Preserve the legacy explicit-dataset opt-in even when an old caller sends scope=none.
    let grounded = kind != "none" || !legacy.is_empty();
    if kind == "none" {
        selected = if grounded { legacy } else { vec![] };
    }
    let mut resolved = Vec::new();
    for value in selected {
        let id = datasets
            .iter()
            .find(|d| d["id"] == value || d["name"] == value)
            .and_then(|d| d["id"].as_str());
        if let Some(id) = id {
            if !resolved.contains(&id.to_owned()) {
                resolved.push(id.to_owned());
            }
        } else {
            warnings.push(json!("dataset_not_found"));
        }
    }
    if grounded && resolved.is_empty() {
        warnings.push(json!("empty_scope"));
    }
    Ok(
        json!({"scope_type":if kind=="datasets"&&resolved.len()==1{"dataset"}else{&kind},"project_ids":project_ids,
        "dataset_ids":resolved,"resolved_dataset_ids":resolved,"document_grounding_enabled":grounded,"source":source,
        "warnings":warnings,"label":request["scope"]["label"],"schema":"les.chat-scope.v1"}),
    )
}
