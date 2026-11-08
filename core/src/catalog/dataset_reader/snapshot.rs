//! A bounded navigation snapshot of committed catalog metadata; no source files or model guesses.
use super::super::{columns, rows, table, Failure, Result};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

fn push_bounded(items: &mut Vec<Value>, bytes: &mut usize, value: &Value, budget: usize) -> bool {
    let size = value.to_string().len();
    if bytes.saturating_add(size) > budget {
        return false;
    }
    *bytes += size;
    items.push(value.clone());
    true
}

pub(super) fn snapshot(db: &Connection, dataset: &str) -> Result<Value> {
    if !table(db,"datasets")? || !db.query_row("SELECT EXISTS(SELECT 1 FROM datasets WHERE id=?1 AND coalesce(dataset_scope,'user')<>'system')",[dataset],|r|r.get::<_,bool>(0))? {
        return Err(Failure::missing("Набор документов не найден."));
    }
    let names = columns(db, "documents")?;
    let path_bytes = if names.iter().any(|n| n == "source_path") {
        "length(CAST(coalesce(source_path,'') AS BLOB))"
    } else {
        "0"
    };
    let size:i64=db.query_row(&format!("SELECT coalesce(sum(length(CAST(file_name AS BLOB))+{path_bytes}),0) FROM documents WHERE dataset_id=?1"),[dataset],|r|r.get(0))?;
    if size > 32 * 1024 * 1024 {
        return Err(Failure::new(
            "READER_CORPUS_LIMIT",
            "Слишком большая карта имён файлов. Разделите набор на части.",
        ));
    }
    let fields = [
        "id",
        "file_name",
        "status",
        "chunk_count",
        "doc_type",
        "content_type",
        "domain",
        "pipeline",
        "file_hash",
        "file_size",
        "file_mtime",
        "source_path",
    ];
    let select = fields
        .iter()
        .map(|name| {
            if names.iter().any(|n| n == name) {
                (*name).to_owned()
            } else {
                format!("NULL AS {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let docs = rows(
        db,
        &format!(
            "SELECT {select} FROM documents WHERE dataset_id=?1 ORDER BY file_name,id LIMIT 50001"
        ),
        [dataset],
    )?;
    if docs.len() > 50000 {
        return Err(Failure::new(
            "READER_CORPUS_LIMIT",
            "Разделите набор на части: карта ограничена 50 000 документов.",
        ));
    }
    let docs = docs
        .into_iter()
        .filter(|d| super::super::library::visible(d["file_name"].as_str().unwrap_or_default()))
        .collect::<Vec<_>>();
    let mut profile = Value::Null;
    if table(db, "les_dataset_profiles")? {
        let raw = super::super::first(
            db,
            "SELECT profile_json FROM les_dataset_profiles WHERE dataset_id=?1",
            [dataset],
        )?;
        profile = raw["profile_json"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null);
    }
    let headings = if table(db, "lexical_chunks")? {
        rows(db,"SELECT doc_name AS file_name,substr(section_heading,1,512) AS heading,count(*) AS chunk_count FROM lexical_chunks
            WHERE dataset_id=?1 AND section_heading IS NOT NULL AND section_heading<>'' GROUP BY doc_name,section_heading
            ORDER BY doc_name,section_heading LIMIT 4001",[dataset])?
    } else {
        vec![]
    };
    let mut identity = json!({"documents":docs,"profile":profile,"headings":headings});
    identity.sort_all_objects();
    let signature = crate::sha256_hex(identity.to_string().as_bytes());
    let mut cards = Vec::new();
    let mut groups = BTreeMap::<String, usize>::new();
    let mut indexed = 0;
    let mut chunks = 0;
    for doc in &docs {
        let name = doc["file_name"].as_str().unwrap_or_default();
        let normalized = name.replace('\\', "/");
        let group = normalized
            .rsplit_once('/')
            .map(|(g, _)| g)
            .unwrap_or(".")
            .to_owned();
        *groups.entry(group).or_default() += 1;
        let count = doc["chunk_count"].as_u64().unwrap_or(0);
        chunks += count;
        indexed += u64::from(doc["status"] == "INDEXED" && count > 0);
        cards.push(
            json!({"file_name":name,"status":doc["status"],"chunk_count":count,
            "file_kind":doc["content_type"],"document_role":doc["doc_type"],"content_layers":[],
            "source_sha256":doc["file_hash"],"navigation_terms":[]}),
        );
    }
    cards.sort_by(|a, b| {
        b["chunk_count"]
            .as_u64()
            .cmp(&a["chunk_count"].as_u64())
            .then_with(|| a["file_name"].as_str().cmp(&b["file_name"].as_str()))
    });
    let mut chosen = Vec::new();
    let mut seen = HashSet::new();
    let mut chosen_names = HashSet::new();
    let mut card_bytes = 0;
    // Cover actual folders first; this is input coverage, never topical routing.
    for card in &cards {
        let normalized = card["file_name"]
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/");
        let group = normalized.rsplit_once('/').map(|(g, _)| g).unwrap_or(".");
        if !seen.contains(group) && push_bounded(&mut chosen, &mut card_bytes, card, 1024 * 1024) {
            seen.insert(group.to_owned());
            chosen_names.insert(card["file_name"].as_str().unwrap_or_default().to_owned());
        }
        if chosen.len() == 500 {
            break;
        }
    }
    for card in &cards {
        if chosen.len() == 500 {
            break;
        }
        if !chosen_names.contains(card["file_name"].as_str().unwrap_or_default())
            && push_bounded(&mut chosen, &mut card_bytes, card, 1024 * 1024)
        {
            chosen_names.insert(card["file_name"].as_str().unwrap_or_default().to_owned());
        }
    }
    let gaps = if indexed < docs.len() as u64 {
        vec![format!(
            "Не все документы готовы к поиску: {indexed} из {}.",
            docs.len()
        )]
    } else {
        vec![]
    };
    let mut group_items = Vec::new();
    let mut group_bytes = 0;
    for (group, files) in groups.iter().take(48) {
        push_bounded(
            &mut group_items,
            &mut group_bytes,
            &json!({"group":group,"files":files}),
            256 * 1024,
        );
    }
    let mut sections = Vec::new();
    let mut section_bytes = 0;
    for heading in headings.iter().take(400) {
        push_bounded(&mut sections, &mut section_bytes, heading, 512 * 1024);
    }
    Ok(
        json!({"schema":"dataset_memory_v1","dataset_id":dataset,"revision_id":format!("native-{signature}"),
        "source_revision":signature,"context_role":"navigation","is_evidence":false,
        "reader_status":"bootstrap","document_count":docs.len(),"indexed_count":indexed,"chunk_count":chunks,
        "dataset_kind":profile["dataset_kind"].as_str().unwrap_or(""),"dataset_kind_label":profile["dataset_kind_label"].as_str().unwrap_or(""),
        "operator_guidance":profile["operator_guidance"].as_str().unwrap_or("").chars().take(1200).collect::<String>(),
        "operator_guidance_role":"navigation_not_evidence","known_gaps":gaps,"file_cards":chosen,
        "file_cards_scope":{"included":chosen.len(),"total":docs.len()},
        "corpus_groups":group_items,
        "corpus_group_total":groups.len(),"sections":sections,"section_total_lower_bound":headings.len()}),
    )
}

pub(super) fn prompt(snapshot: &Value, budget: usize, file_limit: usize) -> Result<Value> {
    let mut input = json!({"schema":"dataset_reader_input_v1","dataset_id":snapshot["dataset_id"],"revision_id":snapshot["revision_id"],
        "document_count":snapshot["document_count"],"indexed_count":snapshot["indexed_count"],"chunk_count":snapshot["chunk_count"],
        "operator_guidance":snapshot["operator_guidance"],"dataset_kind":snapshot["dataset_kind"],"known_gaps":snapshot["known_gaps"],
        "file_cards":snapshot["file_cards"],"sections":snapshot["sections"],"corpus_groups":snapshot["corpus_groups"],
        "file_cards_scope":{"included":0,"total":snapshot["document_count"],"truncated":false},
        "corpus_groups_scope":{"included":0,"total":snapshot["corpus_group_total"]},
        "sections_scope":{"included":0,"total_lower_bound":snapshot["section_total_lower_bound"]},
        "reader_task":{"goal":"navigation only","is_evidence":false,"omitted_files_are_not_absent":true}});
    if let Some(cards) = input["file_cards"].as_array_mut() {
        cards.truncate(file_limit);
    }
    loop {
        input["file_cards_scope"]["included"] =
            json!(input["file_cards"].as_array().map_or(0, Vec::len));
        input["file_cards_scope"]["truncated"] =
            json!(input["file_cards_scope"]["included"] != input["document_count"]);
        input["corpus_groups_scope"]["included"] =
            json!(input["corpus_groups"].as_array().map_or(0, Vec::len));
        input["sections_scope"]["included"] =
            json!(input["sections"].as_array().map_or(0, Vec::len));
        if input.to_string().chars().count() <= budget {
            return Ok(input);
        }
        let mut reduced = false;
        for key in ["sections", "file_cards", "corpus_groups"] {
            if input[key].as_array_mut().is_some_and(|a| {
                if a.is_empty() {
                    false
                } else {
                    a.truncate(a.len() / 2);
                    true
                }
            }) {
                reduced = true;
                break;
            }
        }
        if !reduced {
            return Err(Failure::new(
                "READER_CONTEXT_LIMIT",
                "Карта документов не помещается в контекст модели.",
            ));
        }
    }
}
