//! A reversible wire view: provenance is stored once, never discarded.
use serde_json::{Value, json};
const SCHEMA: &str = "les.evidence-packet.v1";

pub(crate) fn requested() -> Result<bool, &'static str> {
    match std::env::var("LES_EXPERIMENT_EVIDENCE_PACKET").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("") | Ok("legacy-v1") => Ok(false),
        Ok("compact-v1") => Ok(true),
        _ => Err("EVIDENCE_PACKET_EXPERIMENT_INVALID"),
    }
}

pub(crate) fn encode(reply: &Value) -> Result<Value, &'static str> {
    if reply["result"]["evidence_packet"] == SCHEMA {
        decode(reply)?;
        return Ok(reply.clone());
    }
    let mut value = reply.clone();
    let Some(rows) = value["result"]["passages"].as_array_mut() else {
        return Ok(value);
    };
    let mut sources = Vec::new();
    for row in rows {
        let source = row["source"].clone();
        if !source.is_object() || source.get("ref").is_some() {
            return Err("EVIDENCE_PACKET_SOURCE_INVALID");
        }
        let id = sources.len() + 1;
        sources.push(source);
        row["source"] = json!({"ref":id});
    }
    // The legacy envelope requires aligned passage/source arrays. Reject a
    // mismatch rather than quietly removing unbound provenance.
    if reply["sources"].as_array() != Some(&sources) {
        return Err("EVIDENCE_PACKET_SOURCE_INVALID");
    }
    value["result"]["evidence_packet"] = json!(SCHEMA);
    Ok(value)
}

pub(crate) fn decode(reply: &Value) -> Result<Value, &'static str> {
    if reply["result"]["evidence_packet"].is_null() {
        return Ok(reply.clone());
    }
    if reply["result"]["evidence_packet"] != SCHEMA {
        return Err("EVIDENCE_PACKET_SCHEMA_INVALID");
    }
    let sources = reply["sources"]
        .as_array()
        .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
    let mut value = reply.clone();
    let rows = value["result"]["passages"]
        .as_array_mut()
        .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
    for row in rows {
        let reference = row["source"]
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
        let i = reference
            .get("ref")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| n.checked_sub(1))
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?;
        row["source"] = sources
            .get(i)
            .filter(|s| s.is_object() && s.get("ref").is_none())
            .ok_or("EVIDENCE_PACKET_SOURCE_INVALID")?
            .clone();
    }
    value["result"]
        .as_object_mut()
        .unwrap()
        .remove("evidence_packet");
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_is_lossless_for_tables_versions_aliases_and_page_links() {
        let source = json!({"doc_id":"document","doc_name":"Folder/Specification.pdf","source_sha256":"a".repeat(64),"page":3,"url":"/original?revision=a#page=3","locator":{"row":12},"equivalent_sources":[{"source_sha256":"b".repeat(64)}]});
        let reply = json!({"result":{"passages":[{"text":"DN 100 | 17.25 kW","canonical":{"rows":[["DN 100","17.25"]]},"source":source}]},"sources":[source],"warnings":["partial OCR"]});
        let packet = encode(&reply).unwrap();
        assert!(packet.to_string().len() < reply.to_string().len());
        assert_eq!(decode(&packet).unwrap(), reply);
        let mut invalid = packet.clone();
        invalid["result"]["passages"][0]["source"]["ref"] = json!(2);
        assert!(decode(&invalid).is_err());
        invalid["result"]["passages"][0]["source"] = json!({"ref":1,"page":999});
        assert!(decode(&invalid).is_err());
        assert_eq!(encode(&packet).unwrap(), packet);
    }
}
