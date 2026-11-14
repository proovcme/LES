//! A cursor belongs to the ordered reading units, never to hydrated neighbors.
use super::*;

pub(super) struct Page {
    offset: usize,
    total: usize,
}
impl Page {
    pub(super) fn new(offset: usize, total: usize) -> Self {
        Self { offset, total }
    }
    pub(super) fn apply(&self, reply: &mut Value, total_field: &str) -> Result<()> {
        let included = reply["result"]["passages"].as_array().map_or(0, Vec::len);
        let end = self
            .offset
            .checked_add(included)
            .ok_or("SOURCE_OFFSET_INVALID")?;
        if self.offset < self.total && (included == 0 || end > self.total) {
            return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
        }
        reply["result"][total_field] = json!(self.total);
        reply["result"]["offset"] = json!(self.offset);
        reply["result"]["included_fragments"] = json!(included);
        reply["result"]["next_offset"] = if end < self.total {
            json!(end)
        } else {
            Value::Null
        };
        reply["result"]["coverage"] = json!(if included == 0 {
            "no_matches"
        } else if self.offset > 0 || end < self.total {
            "bounded"
        } else {
            "complete"
        });
        Ok(())
    }
}

/// Exhausting a filtered match list never proves the whole document was read.
/// Keep cursor exhaustion separate from document coverage for every reader.
pub(super) fn reading_scope(reply: &mut Value, args: &Value, document_units: usize) {
    let scope = if args["clause"].as_str().is_some_and(|s| !s.is_empty()) {
        "clause"
    } else if args["q"].as_str().is_some_and(|s| !s.trim().is_empty()) {
        "query_matches"
    } else {
        "document"
    };
    reply["result"]["reading_scope"] = json!(scope);
    reply["result"]["document_fragments"] = json!(document_units);
    reply["result"]["matches_exhausted"] = json!(reply["result"]["next_offset"].is_null());
    if scope != "document" && reply["result"]["coverage"] == "complete" {
        reply["result"]["coverage"] = json!("bounded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausting_query_or_clause_matches_never_claims_document_coverage() {
        for args in [json!({"q":"pump"}), json!({"clause":"2.1"})] {
            let mut r = json!({"result":{"passages":[{}]}});
            Page::new(0, 1).apply(&mut r, "total_fragments").unwrap();
            reading_scope(&mut r, &args, 171);
            assert_eq!(r["result"]["coverage"], "bounded");
            assert_eq!(r["result"]["matches_exhausted"], true);
            assert_eq!(r["result"]["document_fragments"], 171);
            assert_eq!(r["result"]["total_fragments"], 1);
            assert!(r["result"]["next_offset"].is_null());
        }
    }
    fn point(i: usize, chars: usize) -> Value {
        json!({"id":format!("p{i}"),"payload":{"text":"x".repeat(chars),"chunk_ord":i}})
    }
    #[test]
    fn unequal_blocks_resume_at_the_first_unread_unit_and_end_with_null() {
        let points = [point(0, 1500), point(1, 1500), point(2, 300), point(3, 300)];
        let mut offset = 0;
        let mut visited = vec![];
        loop {
            let selected = packing::prefix(
                &points[offset..points.len().min(offset + 2)],
                2000,
                Some(10000),
            );
            let mut reply = json!({"result":{"passages":selected}});
            Page::new(offset, points.len())
                .apply(&mut reply, "total_fragments")
                .unwrap();
            visited.extend(
                reply["result"]["passages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["id"].clone()),
            );
            match reply["result"]["next_offset"].as_u64() {
                Some(next) => offset = next as usize,
                None => break,
            }
        }
        assert_eq!(
            visited,
            json!(["p0", "p1", "p2", "p3"]).as_array().unwrap().clone()
        );
        assert_eq!(offset, 3);
        let mut exhausted = json!({"result":{"passages":[]}});
        Page::new(4, 4)
            .apply(&mut exhausted, "total_fragments")
            .unwrap();
        assert!(exhausted["result"]["next_offset"].is_null());
    }
    #[test]
    fn a_non_fitting_first_unit_never_advances_the_cursor() {
        let rows = [point(0, 3000), point(1, 100)];
        let selected = packing::prefix(&rows, 2000, Some(10000));
        assert!(selected.is_empty());
        let mut reply = json!({"result":{"passages":selected}});
        assert_eq!(
            Page::new(0, 2)
                .apply(&mut reply, "total_candidates")
                .unwrap_err()
                .code,
            "CONTEXT_REQUIRED_SECTION_OVERFLOW"
        );
    }
}
