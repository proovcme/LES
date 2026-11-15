//! Explicit copy-only ablation. Unset in ordinary application launches.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) enum Control {
    #[default]
    Off,
    Observe,
    RrfWire,
}
impl Control {
    pub(super) fn requested(args: &Value) -> Result<Self> {
        if args["view"] != "context" {
            return Ok(Self::Off);
        }
        match std::env::var("LES_EXPERIMENT_CONTEXT_ADMISSION").as_deref() {
            Err(std::env::VarError::NotPresent) | Ok("") => Ok(Self::Off),
            Ok("baseline-v1") => Ok(Self::Observe),
            Ok("rrf-wire-v1") => Ok(Self::RrfWire),
            Ok(name @ ("paired-a-v1" | "paired-b-v1")) => {
                Ok(Self::paired(name, args["q"].as_str().unwrap_or("")))
            }
            _ => Err("CONTEXT_EXPERIMENT_INVALID".into()),
        }
    }
    fn paired(period: &str, query: &str) -> Self {
        let first_rrf = les_core::sha256_hex(query.as_bytes()).as_bytes()[0] % 2 == 0;
        if first_rrf == (period == "paired-a-v1") {
            Self::RrfWire
        } else {
            Self::Observe
        }
    }
    pub(super) fn snapshot(self, stage: &str, question: &str, points: &[Value]) {
        if self == Self::Off {
            return;
        }
        let mut sources = BTreeMap::<String, (usize, Option<u64>, usize)>::new();
        for p in points {
            let p = &p["payload"];
            for p in std::iter::once(p).chain(
                p["equivalent_points"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| &a["source"]),
            ) {
                let key = json!([p["dataset_id"], p["source_sha256"]]).to_string();
                let row = sources.entry(key).or_default();
                row.0 += 1;
                if let Some(rank) = p["retrieval_rank"].as_u64() {
                    row.1 = Some(row.1.map_or(rank, |old| old.min(rank)));
                }
                row.2 += usize::from(p["retrieval_expansion"] == true);
            }
        }
        let fingerprints = points
            .iter()
            .map(|p| {
                let canonical =
                    les_core::tool_execution::canonical_json(&p["payload"]["canonical"])?;
                let row = json!([
                    p["id"],
                    p["payload"]["dataset_id"],
                    p["payload"]["source_sha256"],
                    p["payload"]["source_unit_id"],
                    p["payload"]["page"],
                    p["payload"]["retrieval_rank"],
                    p["retrieval_fusion"],
                    p["payload"]["word_snapshot_sha256"],
                    p["payload"]["physical_pdf_read"]["view_sha256"],
                    les_core::sha256_hex(p["payload"]["text"].as_str().unwrap_or("").as_bytes()),
                    les_core::sha256_hex(canonical.as_bytes())
                ]);
                les_core::tool_execution::canonical_json(&row)
                    .map(|text| les_core::sha256_hex(text.as_bytes()))
            })
            .collect::<std::result::Result<Vec<String>, &'static str>>()
            .ok();
        let order = fingerprints
            .as_ref()
            .and_then(|rows| les_core::tool_execution::canonical_json(&json!(rows)).ok())
            .map(|text| les_core::sha256_hex(text.as_bytes()));
        let set = fingerprints
            .and_then(|mut rows| {
                rows.sort();
                les_core::tool_execution::canonical_json(&json!(rows)).ok()
            })
            .map(|text| les_core::sha256_hex(text.as_bytes()));
        eprintln!(
            "{}",
            json!({"event":"context_trial_stage","stage":stage,
            "question_sha256":les_core::sha256_hex(question.as_bytes()),
            "policy":if self==Self::RrfWire{"rrf-wire-v1"}else{"baseline-v1"},
            "units":points.len(),"sources":sources,"order_sha256":order,"set_sha256":set})
        );
    }

    pub(super) fn reply(self, question: &str, reply: &Value) {
        if self == Self::Off {
            return;
        }
        let points = reply["result"]["passages"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|p| {
                std::iter::once(&p["source"]).chain(
                    p["source"]["equivalent_sources"]
                        .as_array()
                        .into_iter()
                        .flatten(),
                )
            })
            .map(|source| {
                json!({"payload":{"dataset_id":source["dataset_id"],
                "source_sha256":source["source_sha256"]}})
            })
            .collect::<Vec<_>>();
        self.snapshot("wire", question, &points);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_periods_flip_every_question_without_consulting_sources_or_gold() {
        for query in [
            "overview",
            "Pressure 17.25",
            "Обзор электроснабжения",
            "東京",
        ] {
            let a = Control::paired("paired-a-v1", query);
            let b = Control::paired("paired-b-v1", query);
            assert_ne!(a, b);
            assert_eq!(a, Control::paired("paired-a-v1", query));
            assert!(matches!(a, Control::Observe | Control::RrfWire));
        }
    }
}

/// One request, one scoped catalog observation per document. No persistent cache.
#[derive(Default)]
pub(super) struct Documents(BTreeMap<String, Value>);
impl Documents {
    pub(super) async fn get(
        &mut self,
        context: &Context,
        turn: &SourceContext,
        args: Value,
    ) -> Result<Value> {
        let key = args.to_string();
        if let Some(value) = self.0.get(&key) {
            return Ok(value.clone());
        }
        let value = document(context, &args).await?;
        scoped_document(turn, &value)?;
        self.0.insert(key, value.clone());
        Ok(value)
    }
}
