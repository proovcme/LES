//! Word span provenance is read from the canonical tree, never inferred from repeated text.
use super::*;
use docling_core::tree::TreeKind;
use std::collections::{HashMap, HashSet};

fn tokens<'a>(text: &'a str, prefix: &str) -> Result<Vec<&'a str>> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(at) = text[offset..].find(prefix) {
        let start = offset + at;
        let suffix = &text[start + prefix.len()..];
        let end = suffix
            .find("END")
            .filter(|n| *n <= 12)
            .ok_or("DOCLING_WORD_INCOMPLETE")?;
        if suffix[..end]
            .strip_prefix('N')
            .is_none_or(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("DOCLING_WORD_INCOMPLETE");
        }
        offset = start + prefix.len() + end + 3;
        out.push(&text[start..offset]);
    }
    Ok(out)
}

pub(super) fn expansions(
    doc: &docling::DoclingDocument,
    rendered: &str,
    prefix: &str,
) -> Result<Value> {
    if prefix.len() != 39 || !prefix.starts_with("LESPART") {
        return Err("DOCLING_WORD_INCOMPLETE");
    }
    let mut proofs: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    if let Some(tree) = &doc.tree {
        for (id, item) in tree.items.iter().enumerate().filter(|(_, i)| !i.deleted) {
            let TreeKind::Table { table, .. } = &item.kind else {
                continue;
            };
            let anchors = table.span_anchors();
            let mut logical: HashMap<String, HashSet<(usize, usize)>> = HashMap::new();
            let mut area = HashMap::<(usize, usize), usize>::new();
            for (r, row) in table.rows.iter().enumerate() {
                for (c, text) in row.iter().enumerate() {
                    let anchor = *anchors
                        .get(r)
                        .and_then(|row| row.get(c))
                        .ok_or("DOCLING_WORD_INCOMPLETE")?;
                    *area.entry(anchor).or_default() += 1;
                    let mut local = HashSet::new();
                    for token in tokens(text, prefix)? {
                        if !local.insert(token) {
                            return Err("DOCLING_WORD_INCOMPLETE");
                        }
                        logical.entry(token.to_owned()).or_default().insert(anchor);
                    }
                }
            }
            for (token, positions) in logical {
                if positions.len() != 1 {
                    eprintln!(
                        "{}",
                        json!({"event":"docling_word_incomplete","stage":"source_logical_cells","cells":positions.len(),"token":token})
                    );
                    return Err("DOCLING_WORD_INCOMPLETE");
                }
                proofs
                    .entry(token)
                    .or_default()
                    .push((id, area[positions.iter().next().unwrap()]));
            }
        }
    }
    // Flat Word furniture and canonical tables serialize merged headers differently.
    // Accept only actual copies inside one table block, bounded by its real span.
    let mut copies: HashMap<String, (usize, HashSet<usize>, bool)> = HashMap::new();
    let mut block = 0;
    let mut in_table = false;
    for line in rendered.lines() {
        let table = line.trim_start().starts_with('|');
        if table && !in_table {
            block += 1;
        }
        in_table = table;
        for token in tokens(line, prefix)? {
            let value = copies.entry(token.to_owned()).or_default();
            value.0 += 1;
            value.1.insert(block);
            value.2 |= !table;
        }
    }
    let mut out = serde_json::Map::new();
    for (token, (count, blocks, outside)) in copies {
        if count == 1 && (outside || !proofs.contains_key(&token)) {
            continue;
        }
        let cells = proofs.get(&token).ok_or_else(||{eprintln!("{}",json!({"event":"docling_word_incomplete","stage":"source_span_missing","copies":count,"token":token}));"DOCLING_WORD_INCOMPLETE"})?;
        let tree = doc.tree.as_ref().ok_or("DOCLING_WORD_INCOMPLETE")?;
        let ancestor = |a: usize, mut b: usize| {
            for _ in 0..128 {
                if a == b {
                    return true;
                }
                let Some(next) = tree.items.get(b).and_then(|i| i.parent) else {
                    return false;
                };
                b = next;
            }
            false
        };
        let mut bound = 1_usize;
        for (i, (id, area)) in cells.iter().enumerate() {
            for (other, _) in &cells[..i] {
                if !ancestor(*id, *other) && !ancestor(*other, *id) {
                    eprintln!(
                        "{}",
                        json!({"event":"docling_word_incomplete","stage":"unrelated_source_tables","token":token})
                    );
                    return Err("DOCLING_WORD_INCOMPLETE");
                }
            }
            bound = bound
                .checked_mul(*area)
                .filter(|n| *n <= 100000)
                .ok_or("DOCLING_WORD_INCOMPLETE")?;
        }
        if outside || blocks.len() != 1 || count > bound {
            eprintln!(
                "{}",
                json!({"event":"docling_word_incomplete","stage":"span_render_count","copies":count,"bound":bound,"outside_table":outside,"table_blocks":blocks.len(),"token":token})
            );
            return Err("DOCLING_WORD_INCOMPLETE");
        }
        out.insert(
            token,
            json!({"copies":count,"logical_cells":1,"span_positions":bound,"table_cell":true}),
        );
    }
    Ok(Value::Object(out))
}
