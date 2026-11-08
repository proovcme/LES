//! Document-local headings, parent windows and navigation identities.
use super::{
    chunks::trim,
    nodes::{defaults, present, sha1, string, MAX_NODES},
};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::LazyLock};
type Result<T> = std::result::Result<T, &'static str>;
static LINES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\r\n|[\n\r\x0b\x0c\x1c-\x1e\u{85}\u{2028}\u{2029}]").unwrap());
static MD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})[\s\x1c-\x1f]+(.{2,160})$").unwrap());
static NUMBERED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d+(?:\.\d+){0,4})[.\s\x1c-\x1f]+([А-ЯЁA-Z].{1,150})$").unwrap()
});
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\x1c-\x1f]+").unwrap());

fn heading(text: &str) -> (String, usize) {
    for line in LINES
        .split(text)
        .take(6)
        .map(trim)
        .filter(|v| !v.is_empty())
    {
        if let Some(value) = MD.captures(line) {
            return (trim(&value[2]).into(), value[1].len());
        }
        if let Some(value) = NUMBERED.captures(line) {
            return (
                format!("{} {}", &value[1], trim(&value[2])),
                value[1].matches('.').count() + 1,
            );
        }
    }
    (String::new(), 0)
}
fn fallback_heading(text: &str) -> String {
    LINES
        .split(text)
        .map(|v| v.trim_matches([' ', '#', '\t']))
        .find(|v| (4..=160).contains(&v.chars().count()))
        .unwrap_or("")
        .into()
}
fn compact(text: &str) -> String {
    let value = SPACES.replace_all(text, " ");
    let value = trim(&value);
    if value.chars().count() <= 1200 {
        value.into()
    } else {
        format!(
            "{}…",
            value.chars().take(1199).collect::<String>().trim_end()
        )
    }
}
fn hierarchy(
    dataset: &str,
    file: &str,
    kind: &str,
    identity: &str,
    ancestors: &[String],
    navigation: bool,
    title: &str,
) -> Value {
    let input = serde_json::to_vec(&json!([
        "les.rag.hierarchy.v1",
        dataset,
        file,
        kind,
        identity
    ]))
    .unwrap();
    let id = format!("{:x}", Sha256::digest(input))[..32].to_owned();
    let mut value = json!({"hierarchy_schema":"les.rag.hierarchy.v1",
        "hierarchy_builder":"deterministic-heading-stack-v1","node_id":id,
        "node_role":if navigation {"navigation"} else {"evidence"},"node_kind":kind,
        "hierarchy_depth":ancestors.len()+1,"ancestor_ids":ancestors,
        "hierarchy_parent_id":ancestors.last().map(String::as_str).unwrap_or(""),
        "evidence_admissible":!navigation});
    if navigation {
        value["navigation_title"] = json!(title);
    }
    value
}

pub(crate) fn context(
    nodes: &mut Vec<Value>,
    dataset: &str,
    file: &str,
    revision: &str,
    window: usize,
) -> Result<()> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_lookup = HashMap::<String, usize>::new();
    let mut group_keys = Vec::new();
    let mut navigation = Vec::new();
    let mut seen_navigation = std::collections::HashSet::new();
    let mut stack: Vec<(usize, String, String)> = Vec::new();
    let (mut last_heading, mut last_level) = (String::new(), 0);
    for (ordinal, node) in nodes.iter_mut().enumerate() {
        let text = node["text"].as_str().unwrap_or("").to_owned();
        if !node["payload"].is_object() {
            node["payload"] = json!({});
        }
        let payload = node["payload"].as_object_mut().unwrap();
        defaults(
            payload,
            json!({"dataset_id":dataset,"file_name":file,"chunk_ord":ordinal,
            "child_ord":ordinal,"content_hash":sha1(&text)}),
        );
        if !revision.is_empty() {
            defaults(payload, json!({"dataset_revision":revision}));
        }
        let (title, level) = heading(&text);
        if !title.is_empty() {
            last_heading = title.clone();
            last_level = level;
            while stack.last().is_some_and(|v| v.0 >= level) {
                stack.pop();
            }
            let identity = stack
                .iter()
                .map(|v| v.1.as_str())
                .chain(std::iter::once(title.as_str()))
                .collect::<Vec<_>>()
                .join("/");
            let ancestors = stack.iter().map(|v| v.2.clone()).collect::<Vec<_>>();
            let meta = hierarchy(
                dataset, file, "section", &identity, &ancestors, true, &title,
            );
            let id = meta["node_id"].as_str().unwrap().to_owned();
            stack.push((level, title.clone(), id.clone()));
            if seen_navigation.insert(id) {
                let mut nav_payload = meta.as_object().unwrap().clone();
                nav_payload.extend(json!({"dataset_id":dataset,"file_name":file,"section_heading":title,
                    "heading_level":level,"content_hash":sha1(&format!("navigation:{dataset}:{file}:{identity}"))}).as_object().unwrap().clone());
                navigation.push(json!({"text":title,"payload":nav_payload}));
            }
            defaults(
                payload,
                json!({"section_heading":title,"heading_level":level}),
            );
        } else if !last_heading.is_empty() {
            defaults(
                payload,
                json!({"section_heading":last_heading,"heading_level":last_level,"heading_inherited":true}),
            );
        } else {
            defaults(payload, json!({"section_heading":fallback_heading(&text)}));
        }
        let source = payload
            .get("source_page")
            .filter(|v| present(v))
            .map(string)
            .unwrap_or_default();
        let identity = format!("{source}:{ordinal}:{}", string(&payload["content_hash"]));
        let kind = if payload.get("type") == Some(&json!("table_row")) {
            "table_row"
        } else {
            "chunk"
        };
        let ancestors = stack.iter().map(|v| v.2.clone()).collect::<Vec<_>>();
        defaults(
            payload,
            hierarchy(dataset, file, kind, &identity, &ancestors, false, ""),
        );
        let page = payload
            .get("source_page")
            .filter(|v| present(v))
            .or_else(|| payload.get("page").filter(|v| present(v)))
            .or_else(|| payload.get("page_number"));
        let (key, context_kind) = if let Some(page) = page.filter(|v| !v.is_null()) {
            let table = payload
                .get("table_index")
                .filter(|v| present(v))
                .map(string)
                .unwrap_or_default();
            (
                format!("page:{}:table:{table}", string(page)),
                if kind == "table_row" {
                    "table_page"
                } else {
                    "pdf_page"
                },
            )
        } else {
            (
                format!("window:{}", ordinal / window.max(1)),
                "markdown_window",
            )
        };
        let index = if let Some(index) = group_lookup.get(&key) {
            *index
        } else {
            let index = groups.len();
            group_lookup.insert(key.clone(), index);
            group_keys.push(key);
            groups.push(Vec::new());
            index
        };
        groups[index].push(ordinal);
        defaults(payload, json!({"context_kind":context_kind}));
    }
    for (ordinal, indexes) in groups.iter().enumerate() {
        let id = sha1(&format!("{dataset}:{file}:{}", group_keys[ordinal]))[..24].to_owned();
        let title = indexes
            .iter()
            .map(|&i| &nodes[i]["payload"]["section_heading"])
            .find(|v| present(v))
            .map(string)
            .unwrap_or_default();
        for &index in indexes {
            defaults(
                nodes[index]["payload"].as_object_mut().unwrap(),
                json!({"parent_id":id,"parent_ord":ordinal,"parent_heading":title}),
            );
        }
    }
    for index in 0..nodes.len() {
        let parent = nodes[index]["payload"]["parent_id"].clone();
        let before = if index > 0 && nodes[index - 1]["payload"]["parent_id"] == parent {
            Some(compact(nodes[index - 1]["text"].as_str().unwrap_or("")))
        } else {
            None
        };
        let after = if index + 1 < nodes.len() && nodes[index + 1]["payload"]["parent_id"] == parent
        {
            Some(compact(nodes[index + 1]["text"].as_str().unwrap_or("")))
        } else {
            None
        };
        let payload = nodes[index]["payload"].as_object_mut().unwrap();
        if let Some(value) = before {
            defaults(payload, json!({"context_before":value}));
        }
        if let Some(value) = after {
            defaults(payload, json!({"context_after":value}));
        }
    }
    if nodes.len() + navigation.len() > MAX_NODES {
        return Err("INDEX_FILE_CHUNK_LIMIT");
    }
    nodes.extend(navigation);
    Ok(())
}
