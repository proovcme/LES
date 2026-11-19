//! Source identities and bounded model-facing evidence projections.
use super::Result;
use serde_json::{json, Value};
mod pages;
#[derive(Default)]
pub(super) struct Evidence {
    pub(super) sources: Vec<Value>,
    blocks: Vec<Value>,
    initial: Vec<(usize, Value)>,
    retained: Vec<(usize, Value)>,
    verified_reads: std::collections::BTreeSet<u64>,
    explicit_reads: std::collections::BTreeSet<u64>,
    search_heads: std::collections::BTreeSet<u64>,
    required: bool,
    retried: bool,
    delivered: std::collections::BTreeSet<u64>,
    pages: pages::Pages,
    pub(super) read_failures: Vec<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separate_file_questions_focus_terminal_citations_without_renumbering() {
        let files = [json!("Pump record.docx"), json!("Valve model.pdf")];
        let mut evidence = Evidence {
            sources: [0, 0, 1, 1, 1]
                .into_iter()
                .map(|file| json!({"doc_name":files[file]}))
                .collect(),
            ..Evidence::default()
        };
        let blocks = [
            "pressure",
            "pressure 7 bar",
            "diameter 50 mm",
            "diameter 60 mm",
            "model",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, body)| json!({"source_index":i+1,"text":body,"body_offset":0}))
        .collect::<Vec<_>>();
        let question =
            "From Pump record report pressure in bar. From Valve model report diameter in mm.";
        let selected = evidence.citation_focus(question, &files, &blocks).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|block| block["source_index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert!(evidence
            .citation_focus("Compare these documents.", &files, &blocks)
            .is_none());
        // Text from a tool the model chose to call stays eligible in full.
        evidence.explicit_reads.insert(1);
        assert!(evidence.citation_focus(question, &files, &blocks).is_none());
    }

    #[test]
    fn ready_origins_update_prefetch_without_repeating_body_or_losing_chat_pins() {
        let source = json!({"doc_id":"first","doc_name":"First.pdf","dataset_id":"scope",
            "source_sha256":"a".repeat(64),"page":1,"qdrant_point_id":"point"});
        let mut reply = json!({"tool":"search_sources","status":"ok","operation":"indexed_sections",
            "sources":[source],"result":{"index_revision":"r",
                "read_receipt":{"schema":"les.indexed-read.v1","index_revision":"r","point_ids":["point"]},
                "passages":[{"text":"Exact rating 17.25 kW","source":source}]}});
        let mut evidence = Evidence::new(true);
        evidence.add(&reply);
        reply["sources"][0]["prepared_evidence_origin"] = json!(true);
        reply["sources"][0]["equivalent_sources"] = json!([{"doc_id":"second","doc_name":"Second.docx",
            "dataset_id":"scope","source_sha256":"b".repeat(64),"ordinal":17,"read_offset":21,
            "view_sha256":"c".repeat(64),"evidence_kind":"physical_word_section"}]);
        reply["result"]["passages"][0]["source"] = reply["sources"][0].clone();
        evidence.add(&reply);
        assert_eq!(evidence.sources.len(), 1);
        assert_eq!(evidence.initial.len(), 1);
        let block = &evidence.initial[0].1;
        let text = Evidence::text(block);
        assert_eq!(text.matches("Exact rating 17.25 kW").count(), 1);
        for expected in [
            "Second.docx",
            "second",
            &"b".repeat(64),
            &"c".repeat(64),
            "\"read_offset\":21",
        ] {
            assert!(text.contains(expected), "{expected}");
        }
        assert_eq!(
            &text[block["body_offset"].as_u64().unwrap() as usize..],
            "Exact rating 17.25 kW"
        );
        assert_eq!(evidence.sources[0]["equivalent_sources"][0]["ordinal"], 17);
    }
    #[test]
    fn compact_packet_admits_identical_blocks_and_citations_in_the_main_chat() {
        let source = json!({"doc_id":"d","dataset_id":"ds","doc_name":"Book.pdf","source_sha256":"a".repeat(64),"page":2,"evidence_kind":"physical_pdf_page"});
        let raw = json!({"tool":"search_sources","status":"ok","result":{"passages":[{"text":"Exact quantity 17.25 kW","source":source}]},"sources":[source]});
        let compact = super::super::tools::sources::packet::encode(&raw).unwrap();
        let mut baseline = Evidence::new(true);
        baseline.add(&raw);
        let mut candidate = Evidence::new(true);
        candidate.add(&compact);
        assert_eq!(candidate.sources.len(), 1);
        assert_eq!(candidate.sources, baseline.sources);
        assert_eq!(candidate.blocks, baseline.blocks);
        assert!(!candidate.initial.is_empty());
        assert_eq!(candidate.initial, baseline.initial);
        assert_eq!(
            candidate.projection(&compact, 12000),
            baseline.projection(&raw, 12000)
        );
        let mut invalid = compact;
        invalid["result"]["passages"][0]["source"]["ref"] = json!(999);
        let mut rejected = Evidence::new(true);
        rejected.add(&invalid);
        assert!(rejected.sources.is_empty());
        assert_eq!(
            rejected.read_failures[0]["code"],
            "EVIDENCE_PACKET_SOURCE_INVALID"
        );
    }
    #[test]
    fn partial_conversion_keeps_verified_text_and_exposes_unread_sources_without_proof() {
        let mut flow = Evidence::new(true);
        flow.add(&json!({"tool":"read_source","status":"ok","result":{
            "passages":[{"text":"Exact quantity 17.25","source":{"doc_id":"read","source_sha256":"r","retrieval_incomplete":true,"unread_visual_count":2}}],
            "read_failures":[{"source":{"doc_id":"unread"},"code":"WORKER_TIMEOUT","evidence_admissible":false}]}}));
        assert_eq!(flow.read_failures.len(), 1);
        assert_eq!(flow.sources.len(), 1);
        let blocks = flow.required_blocks();
        assert_eq!(blocks.len(), 1);
        assert!(Evidence::text(&blocks[0]).contains("Обзор неполный"));
        assert!(Evidence::text(&blocks[0]).contains("Объектов без распознанного текста: 2"));
        assert!(Evidence::text(&blocks[0]).ends_with("Exact quantity 17.25"));
        assert_eq!(flow.sources[0]["doc_id"], "read");
    }
    #[test]
    fn word_units_do_not_collapse_and_legacy_discovery_never_becomes_proof() {
        let mut flow = Evidence::new(true);
        let source = |i| json!({"doc_id":"word","source_sha256":"a".repeat(64),"view_sha256":"b".repeat(64),"ordinal":i});
        flow.add(
            &json!({"tool":"read_source","status":"ok","result":{"passages":[
            {"text":"Header and first complete row","source":source(0)},
            {"text":"Header and second complete row","source":source(1)}]}}),
        );
        assert_eq!(flow.sources.len(), 2);
        assert_eq!(flow.required_blocks().len(), 2);
        let mut legacy = source(2);
        legacy["evidence_admissible"] = json!(false);
        flow.add(&json!({"tool":"search_sources","status":"ok","operation":"indexed_sections", "result":{
            "passages":[{"text":"Severed legacy row","source":legacy}],"index_revision":"r",
            "read_receipt":{"schema":"les.indexed-read.v1","index_revision":"r","point_ids":["x"]}}}));
        assert_eq!(flow.sources.len(), 2);
        assert!(!json!(flow.optional_blocks())
            .to_string()
            .contains("Severed legacy row"));
    }
    fn page_part(offset: usize, text: &str, total: usize) -> Value {
        let end = offset + text.chars().count();
        let next = (end < total).then_some(end);
        json!({"tool":"read_pdf_source","status":"ok","result":{"text":text,"offset":offset,
            "next_offset":next,"total_chars":total,"complete":offset==0 && next.is_none(),
            "selection":"page_range","extractor":"les.pdfium.page.v1","page":1,"source_sha256":"a".repeat(64)},
            "sources":[{"doc_id":"doc","dataset_id":"ds","doc_name":"Source.pdf","source_sha256":"a".repeat(64),
                "evidence_kind":"physical_pdf_page","page":1,"offset":offset,"next_offset":next}]})
    }
    #[test]
    fn physical_page_transfers_form_one_read_before_words_and_table_rows_are_interpreted() {
        let left = "Введение\n|Name|Unit|Qty|\n|---|---|---|\n|Громкоговор";
        let right = "итель🙂|шт.|2|\n";
        let full = format!("{left}{right}");
        let first = page_part(0, left, full.chars().count());
        let last = page_part(left.chars().count(), right, full.chars().count());
        let originals = (first.clone(), last.clone());
        let mut flow = Evidence::new(true);
        flow.add(&first);
        flow.add(&last);
        assert_eq!(flow.sources.len(), 1);
        assert!(flow.optional_blocks().is_empty());
        let required = flow.required_blocks();
        assert_eq!(required.len(), 1);
        let block = &required[0];
        assert_eq!(
            &Evidence::text(block)[block["body_offset"].as_u64().unwrap() as usize..],
            full
        );
        assert_eq!(flow.sources[0]["page_text_complete"], true);
        assert_eq!(flow.sources[0]["assembled_part_count"], 2);
        assert!(flow.sources[0]["next_offset"].is_null());
        let projected = flow.projection(&last, 4000);
        assert_eq!(projected["sources"][0]["index"], 1);
        assert_eq!(projected["result"]["text"], "[Источник 1]");
        flow.confirm_delivery(&json!([{"index":1}]));
        assert!(flow.can_answer(&json!([{"index":1}])));
        assert_eq!((first, last), originals); // Durable tool receipts stay unmodified.
    }
    #[test]
    fn page_assembly_never_bridges_missing_ranges_or_other_origins() {
        for variant in ["gap", "document", "revision", "page", "literal"] {
            let mut flow = Evidence::new(true);
            flow.add(&page_part(0, "abcde", 10));
            let mut suffix = if variant == "gap" {
                page_part(6, "ghij", 10)
            } else {
                page_part(5, "fghij", 10)
            };
            match variant {
                "document" => suffix["sources"][0]["doc_id"] = json!("other"),
                "revision" => {
                    suffix["sources"][0]["source_sha256"] = json!("b".repeat(64));
                    suffix["result"]["source_sha256"] = json!("b".repeat(64));
                }
                "page" => {
                    suffix["sources"][0]["page"] = json!(2);
                    suffix["result"]["page"] = json!(2);
                }
                "literal" => suffix["result"]["selection"] = json!("literal_match"),
                _ => (),
            }
            flow.add(&suffix);
            assert_eq!(flow.sources.len(), 2, "{variant}");
            assert!(
                !json!(flow.required_blocks())
                    .to_string()
                    .contains("abcdefghij"),
                "{variant}"
            );
            assert!(
                flow.sources.iter().all(|s| s["page_text_complete"] != true),
                "{variant}"
            );
        }
    }
    #[test]
    fn explicit_read_keeps_prior_native_reads_and_continuations_for_comparison() {
        let passage = |doc: &str, page: u64, text: &str| json!({"text":text,"source":{"doc_id":doc,"source_sha256":"r1","page":page,"qdrant_point_id":format!("{doc}-{page}")}});
        let mut evidence = Evidence::new(true);
        evidence.add(&json!({"tool":"search_sources","status":"ok","operation":"indexed_sections","result":{
            "index_revision":"generation-1","read_receipt":{"schema":"les.indexed-read.v1","index_revision":"generation-1","point_ids":["A-1","A-2"]},
            "passages":[passage("A",1,"A first"),passage("A",2,"A continuation")]}}));
        assert!(evidence.has_reads());
        evidence.add(&json!({"tool":"read_source","status":"ok","result":{"passages":[passage("B",1,"B comparison")]}}));
        let optional = json!(evidence.optional_blocks()).to_string();
        assert!(optional.contains("A first") && optional.contains("A continuation"));
        assert!(json!(evidence.required_blocks())
            .to_string()
            .contains("B comparison"));
        evidence.add(&json!({"tool":"read_source","status":"ok","result":{"passages":[passage("A",1,"A first")]}}));
        let optional = json!(evidence.optional_blocks()).to_string();
        assert!(optional.contains("B comparison") && optional.contains("A continuation"));
        assert_eq!(evidence.sources.len(), 3);
    }
    #[test]
    fn an_unread_catalog_unit_cannot_request_a_larger_answer_window() {
        let passages:Vec<_>=(1..=2).map(|page|json!({"text":"Actual page","source":{"doc_id":"doc","page":page,"context_unit":"revision","document_fragment_count":2}})).collect();
        let mut result =
            json!({"tool":"search_sources","status":"ok","result":{"passages":passages}});
        let mut evidence = Evidence::new(true);
        evidence.add(&result);
        assert!(!evidence.read_omitted(&json!([])));
        result["tool"] = json!("read_source");
        evidence.add(&result);
        assert!(evidence.read_omitted(&json!([{"index":1}])));
        assert!(!evidence.read_omitted(&json!([{"index":1},{"index":2}])));
    }
    #[test]
    fn verified_partial_read_can_use_a_larger_window_without_claiming_a_whole_document() {
        let mut evidence = Evidence::new(true);
        evidence.add(
            &json!({"tool":"read_source","status":"ok","result":{"passages":[
                {"text":"First bounded proof","source":{"doc_id":"doc","page":1}},
                {"text":"Continuation proof","source":{"doc_id":"doc","page":2}}
            ]}}),
        );
        assert!(evidence.sources.iter().all(|s| s["context_unit"].is_null()));
        assert!(evidence.read_omitted(&json!([{"index":1}])));
        assert!(!evidence.read_omitted(&json!([{"index":1},{"index":2}])));
    }
    #[test]
    fn catalog_metadata_stays_bounded_after_a_source_has_already_been_read() {
        let mut evidence = Evidence::new(true);
        evidence.add(
            &json!({"tool":"read_source","status":"ok","result":{"text":"Actual proof"},
            "sources":[{"doc_id":"doc","doc_name":"Source.txt"}]}),
        );
        evidence.confirm_delivery(&json!([{"index":1}]));
        assert!(evidence.can_answer(&json!([{"index":1}])));
        for tool in ["search_sources", "dataset_map"] {
            let raw = json!({"schema":"les_tool_result_v1","tool":tool,"status":"ok","result":{
                "documents":(0..50).map(|i|json!({"id":i.to_string(),"file_name":format!("{}/File-{i}.txt","folder/".repeat(40)),"text":"NOT CATALOG DATA"})).collect::<Vec<_>>(),
                "total":50,"offset":0},"sources":[]});
            let original = raw.clone();
            let projected = evidence.projection(&raw, 2400);
            let result = &projected["result"];
            assert!(result.to_string().chars().count() <= 1200);
            let count = result["documents"].as_array().unwrap().len();
            assert!(count > 0 && count < 50);
            assert_eq!(result["next_offset"], count);
            assert!(!projected.to_string().contains("NOT CATALOG DATA"));
            assert_eq!(original, raw);
            let wrapped =
                json!({"tool":"use_extensions_test","operation":"call","status":"ok","result":raw});
            assert_eq!(evidence.projection(&wrapped, 2400)["result"], projected);
        }
    }
    #[test]
    fn a_read_receipt_must_be_delivered_in_the_current_window_before_finalization() {
        let mut flow = Evidence::new(true);
        flow.add(&json!({"tool":"read_pdf_source","status":"ok","result":{"text":"Actual page"},
            "sources":[{"doc_id":"doc","doc_name":"Source.pdf","source_sha256":"a".repeat(64),"page":1}]}));
        assert!(flow.has_reads() && !flow.needs_read());
        assert!(!flow.can_answer(&json!([{"index":1}])));
        flow.confirm_delivery(&json!([{"index":2}]));
        assert!(!flow.can_answer(&json!([{"index":2}])));
        flow.confirm_delivery(&json!([{"index":1}]));
        assert!(flow.can_answer(&json!([{"index":1}])));
        flow.add(&json!({"tool":"read_pdf_source","status":"ok","result":{"text":"Next actual page"},
            "sources":[{"doc_id":"doc","doc_name":"Source.pdf","source_sha256":"a".repeat(64),"page":2}]}));
        assert!(!flow.can_answer(&json!([{"index":1}])));
        flow.confirm_delivery(&json!([]));
        assert!(!flow.can_answer(&json!([{"index":1}])));
        assert!(flow.retry_read_once());
        assert!(!flow.retry_read_once());
    }
    #[test]
    fn discovery_manifests_failed_reads_and_blank_text_do_not_advance_to_answering() {
        let mut flow = Evidence::new(true);
        for result in [
            json!({"tool":"search_sources","status":"ok","result":{"text":"Candidate fact"},"sources":[{"doc_id":"doc"}]}),
            json!({"tool":"read_source","status":"ok","operation":"parts","result":{"parts":[{"name":"figure.png"}]},"sources":[{"doc_id":"doc"}]}),
            json!({"tool":"read_pdf_source","status":"error","result":{"text":"Not proof","code":"PDF_CORRUPT"},"sources":[{"doc_id":"doc"}]}),
            json!({"tool":"read_source","status":"ok","result":{"text":"  "},"sources":[{"doc_id":"doc"}]}),
        ] {
            flow.add(&result);
            flow.confirm_delivery(&json!([{"index":1}]));
            assert!(
                flow.needs_read() && !flow.has_reads() && !flow.can_answer(&json!([{"index":1}]))
            );
        }
        assert!(Evidence::new(false).can_answer(&json!([])));
    }
    #[test]
    fn nested_extension_reads_use_the_same_content_admission_and_delivery_gate() {
        let mut flow = Evidence::new(true);
        let read = json!({"tool":"read_source","status":"ok","result":{"text":"Actual source"},"sources":[{"doc_id":"doc","doc_name":"Source.txt"}]});
        let wrap = |result| json!({"tool":"use_extensions_test","status":"ok","operation":"call","result":result});
        let mut read = read;
        read["schema"] = json!("les_tool_result_v1");
        let mut inner = wrap(read);
        inner["schema"] = json!("les_tool_result_v1");
        flow.add(&wrap(inner));
        flow.confirm_delivery(&json!([{"index":1}]));
        assert!(flow.can_answer(&json!([{"index":1}])));
    }
    #[test]
    fn coordinator_reads_count_as_reads_without_forcing_a_model_tool_call() {
        let mut flow = Evidence::new(true);
        let source = json!({"doc_id":"doc","dataset_id":"ds","doc_name":"Source.txt","qdrant_point_id":"p1","source_sha256":"a".repeat(64)});
        let read = json!({"tool":"search_sources","status":"ok","operation":"indexed_sections",
            "result":{"index_revision":"generation-1","read_receipt":{"schema":"les.indexed-read.v1","index_revision":"generation-1","point_ids":["p1"]},
                "passages":[{"text":"Actual indexed section","source":source}]},"sources":[source]});
        flow.add(&read);
        assert!(flow.has_reads() && !flow.needs_read());
        assert_eq!(flow.optional_blocks().len(), 1);
        flow.confirm_delivery(&json!([{"index":1}]));
        assert!(flow.can_answer(&json!([{"index":1}])));
        let mut stale = read;
        stale["result"]["read_receipt"]["index_revision"] = json!("generation-0");
        let mut rejected = Evidence::new(true);
        rejected.add(&stale);
        assert!(rejected.needs_read() && rejected.optional_blocks().is_empty());
    }
    #[test]
    fn rotating_a_block_does_not_turn_unadmitted_tool_text_into_optional_proof() {
        let result = |tool, doc, text| json!({"status":"ok","tool":tool,"result":{"text":text},"sources":[{"doc_id":doc,"doc_name":"Source.txt"}]});
        let mut evidence = Evidence::new(true);
        evidence.add(&result("read_source", "first", "Actual first read"));
        evidence.add(&result("other_tool", "untrusted", "Unadmitted output"));
        assert!(evidence.required_blocks().is_empty());
        evidence.add(&result("read_source", "last", "Actual last read"));
        let optional = evidence.optional_blocks();
        assert_eq!(optional.len(), 1);
        assert!(Evidence::text(&optional[0]).contains("Actual first read"));
        assert!(!serde_json::to_string(&optional)
            .unwrap()
            .contains("Unadmitted output"));
        assert_eq!(evidence.required_blocks().len(), 1);
    }
    #[test]
    fn successive_reads_move_whole_older_proofs_to_optional_context_and_can_restore_them() {
        let read = |page, text| json!({"status":"ok","tool":"read_pdf_source","result":{"text":text},"sources":[{"doc_id":"doc","doc_name":"Source.pdf","source_sha256":"a".repeat(64),"page":page,"offset":0}]});
        let mut evidence = Evidence::default();
        evidence.add(&read(1, "First page proof"));
        evidence.add(&read(2, "Second page proof"));
        assert_eq!(evidence.blocks.len(), 1);
        assert!(Evidence::text(&evidence.blocks[0]).contains("Second page proof"));
        assert_eq!(evidence.optional_blocks().len(), 1);
        assert!(Evidence::text(&evidence.optional_blocks()[0]).contains("First page proof"));
        evidence.add(&read(1, "First page proof"));
        assert_eq!(evidence.sources.len(), 2);
        assert_eq!(evidence.blocks.len(), 1);
        assert!(Evidence::text(&evidence.blocks[0]).contains("First page proof"));
        assert!(Evidence::text(&evidence.optional_blocks()[0]).contains("Second page proof"));
        let before = evidence.blocks.clone();
        evidence.add(
            &json!({"status":"error","tool":"read_pdf_source","result":{"code":"PDF_CORRUPT"}}),
        );
        assert_eq!(evidence.blocks, before);
    }
    #[test]
    fn final_window_keeps_distinct_read_documents_before_extra_fragments() {
        let passage = |doc, part, text: &str| {
            json!({"text":text,"source":{
                "doc_id":doc,"doc_name":doc,"dataset_id":"scope","qdrant_point_id":part
            }})
        };
        let mut evidence = Evidence::new(true);
        evidence.add(
            &json!({"tool":"read_source","status":"ok","result":{"passages":[
                passage("A", "a", "FIRST_DOCUMENT")
            ]}}),
        );
        evidence.add(
            &json!({"tool":"read_source","status":"ok","result":{"passages":[
                passage("B", "b1", "SECOND_DOCUMENT"),
                passage("B", "b2", &format!("EXTRA {}", "text ".repeat(180)))
            ]}}),
        );
        let build = |limit| {
            les_core::chat_context::execute(&json!({
            "action":"assemble",
            "preset":{"preset_id":"test","input_limit":limit,"generation_reserve":20,"safety_reserve":20},
            "profile_prefix":"Use the visible evidence.",
            "request_payload":{"question":"Compare both sources"},
            "evidence":evidence.answer_blocks(2).unwrap(),
            "source_map":evidence.sources
        })).unwrap()
        };
        let packed = build(250);
        let wire = packed["messages"].to_string();
        assert!(wire.contains("FIRST_DOCUMENT") && wire.contains("SECOND_DOCUMENT"));
        assert!(!wire.contains("EXTRA"));
        let visible = les_core::chat_context::execute(&json!({
            "action":"visible","packet":packed["packet"],"source_map":evidence.sources
        }))
        .unwrap();
        assert_eq!(
            evidence.omitted_explicit_reads(&visible),
            vec![json!({"index":3,"doc_id":"B","label":"B"})]
        );
    }
    #[test]
    fn pull_keeps_the_leading_evidence_from_each_verified_search() {
        let search = |doc: &str, parts: &[&str]| {
            json!({
                "tool":"search_sources","status":"ok","operation":"indexed_sections","result":{
                    "pull_search":true,"index_revision":"rev",
                    "read_receipt":{"schema":"les.indexed-read.v1","index_revision":"rev","point_ids":parts},
                    "passages":parts.iter().map(|part| json!({"text":part,"source":{
                        "doc_id":doc,"doc_name":doc,"dataset_id":"scope","qdrant_point_id":part
                    }})).collect::<Vec<_>>()
                }
            })
        };
        let mut evidence = Evidence::new(true);
        evidence.add(&search("one", &["A", "extra"]));
        evidence.add(&search("two", &["B"]));
        let blocks = evidence.answer_blocks(2).unwrap();
        assert_eq!(
            blocks
                .iter()
                .map(|b| b["source_index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 3, 2]
        );
    }
    #[test]
    fn search_is_optional_but_read_promotes_text_and_keeps_exact_source_identity() {
        let source = json!({"doc_id":"catalog-doc","dataset_id":"ds","doc_name":format!("{}/Source.pdf","long-folder/".repeat(30)),"source_sha256":"a".repeat(64),"page":3,"qdrant_point_id":"point"});
        let mut envelope = json!({"status":"ok","tool":"search_sources","result":{"passages":[{"text":"Exact proof 17.35 kW","source":source}]},"sources":[source]});
        let canonical = json!({"schema":"les.document-structure.v1","kind":"paragraph","text":"Exact proof 17.35 kW"});
        envelope["result"]["passages"][0]["canonical"] = canonical.clone();
        let mut evidence = Evidence::default();
        evidence.add(&envelope);
        assert!(evidence.blocks.is_empty());
        assert_eq!(evidence.optional_blocks().len(), 1);
        assert_eq!(evidence.optional_blocks()[0]["canonical"], canonical);
        let projected = evidence.projection(&envelope, 32000);
        assert!(projected.get("sources").is_none());
        assert_eq!(
            projected["result"]["passages"][0]["source"]["doc_id"],
            "catalog-doc"
        );
        assert!(!projected.to_string().contains("Exact proof"));
        assert!(!projected.to_string().contains("long-folder"));
        envelope["tool"] = json!("read_source");
        evidence.add(&envelope);
        assert!(evidence.optional_blocks().is_empty());
        assert_eq!(evidence.blocks[0]["canonical"], canonical);
        let text = Evidence::text(&evidence.blocks[0]);
        assert!(
            text.contains("catalog-doc")
                && text.contains(&"a".repeat(64))
                && text.contains("Exact proof 17.35 kW")
        );
        let mut terminal = evidence.blocks[0].clone();
        Evidence::terminal_block(&mut terminal);
        let text = Evidence::text(&terminal);
        assert!(text.starts_with("[Источник 1 | Source.pdf · стр. 3]\n"));
        assert!(!text.contains("catalog-doc") && !text.contains("source_sha256"));
        assert_eq!(
            &text[terminal["body_offset"].as_u64().unwrap() as usize..],
            "Exact proof 17.35 kW"
        );
        assert_eq!(terminal["source_index"], 1);
        assert_eq!(terminal["canonical"], canonical);
        assert_eq!(evidence.sources[0]["doc_id"], "catalog-doc");
    }
}
impl Evidence {
    pub(super) fn new(required: bool) -> Self {
        Self {
            required,
            ..Self::default()
        }
    }
    pub(super) fn has_reads(&self) -> bool {
        !self.verified_reads.is_empty()
    }
    pub(super) fn needs_read(&self) -> bool {
        self.required && !self.has_reads()
    }
    pub(super) fn retry_used(&self) -> bool {
        self.retried
    }
    pub(super) fn read_omitted(&self, visible: &Value) -> bool {
        self.sources.iter().any(|source| {
            // Delivery is independent of the full-document coverage claim.
            // A partial but verified read can also be omitted while tool
            // schemas reserve space, and must remain eligible for the one
            // existing answer-window retry.
            source["index"]
                .as_u64()
                .is_some_and(|i| self.verified_reads.contains(&i))
                && !visible
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|s| s["index"] == source["index"])
        })
    }
    pub(super) fn omitted_explicit_reads(&self, visible: &Value) -> Vec<Value> {
        self.blocks
            .iter()
            .chain(self.retained.iter().map(|(_, block)| block))
            .chain(self.initial.iter().map(|(_, block)| block))
            .filter_map(|block| block["source_index"].as_u64())
            .filter(|index| {
                (self.explicit_reads.contains(index) || self.search_heads.contains(index))
                    && !visible
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|source| source["index"] == *index)
            })
            .filter_map(|index| self.sources.get(index as usize - 1))
            .map(|source| {
                let name = source["doc_name"].as_str().unwrap_or("Документ");
                let label = source["label"].as_str().unwrap_or(name);
                let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
                json!({"index":source["index"],"doc_id":source["doc_id"],"label":format!("{file}{}", label.strip_prefix(name).unwrap_or(""))})
            })
            .collect()
    }
    pub(super) fn confirm_delivery(&mut self, visible: &Value) {
        self.delivered = visible
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["index"].as_u64())
            .filter(|i| self.verified_reads.contains(i))
            .collect();
    }
    pub(super) fn can_answer(&self, visible: &Value) -> bool {
        !self.required
            || visible
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["index"].as_u64())
                .any(|i| self.delivered.contains(&i))
    }
    pub(super) fn retry_read_once(&mut self) -> bool {
        if self.retried {
            return false;
        }
        self.retried = true;
        true
    }
    pub(super) fn reading_trace(&self) -> Value {
        json!({"required":self.required,"performed":self.has_reads(),"retry":self.retried,
            "phase":if !self.required {"not_required"} else if !self.has_reads() {"discovery"} else if self.delivered.is_empty() {"read"} else {"ready"},
            "read_indexes":self.verified_reads,"delivered_read_indexes":self.delivered})
    }
    fn text(block: &Value) -> &str {
        block
            .as_str()
            .or_else(|| block["text"].as_str())
            .unwrap_or("")
    }
    fn block(source: &Value, text: &str, canonical: &Value) -> Value {
        let mut note = if source["ocr"] == true {
            "Текст получен OCR: возможны ошибки знаков, чисел и структуры таблицы. Для точных выводов проверь оригинал.\n"
        } else {
            ""
        }.to_owned();
        if source["retrieval_incomplete"] == true {
            note.push_str("Часть найденных оригиналов не дочитана из-за ошибки. Обзор неполный: отвечай только по прочитанному, не утверждай, что в непрочитанных файлах нет ответа.\n");
        }
        if source["evidence_kind"] == "physical_pdf_unit" {
            note.push_str("Это отдельная структурная часть страницы, а не вся страница. Общие выводы о требованиях и исключениях требуют дочитывания оригинала через read_pdf_source.\n");
        }
        if let Some(code) = source["table_projection_warning"].as_str() {
            note.push_str(crate::document_parser::projection_notice(code));
            note.push('\n');
        }
        let group = &source["workbook_group"];
        if let (Some(position), Some(row), Some(title)) = (
            group["position"].as_str(),
            group["row"].as_u64(),
            group["title"].as_str(),
        ) {
            note.push_str(&format!(
                "Группа этой строки (позиция {position}, строка {row} того же листа): {title}\n"
            ));
        }
        if source["visual_content"] == true {
            note.push_str("Документ содержит рисунки/вложения. Текстовая выдержка не подтверждает геометрию, графики и детали изображений. Оригиналы доступны через read_source view=parts; OCR не гарантирует чтение всех надписей.\n");
        }
        if let Some(count) = source["unread_visual_count"].as_u64().filter(|n| *n > 0) {
            note.push_str(&format!("Объектов без распознанного текста: {count}. Их содержимое не подтверждено этой выдержкой.\n"));
        }
        if source["prepared_evidence_origin"] == true {
            if let Some(origins) = source["equivalent_sources"]
                .as_array()
                .filter(|v| !v.is_empty())
            {
                let identities = origins
                    .iter()
                    .map(|origin| {
                        let fields = [
                            "doc_id",
                            "doc_name",
                            "dataset_id",
                            "source_sha256",
                            "page",
                            "ordinal",
                            "view_sha256",
                            "read_offset",
                            "offset",
                            "sheet_name",
                            "row",
                            "evidence_kind",
                        ]
                        .into_iter()
                        .filter(|key| !origin[*key].is_null())
                        .map(|key| (key.to_owned(), origin[key].clone()))
                        .collect::<serde_json::Map<_, _>>();
                        Value::Object(fields)
                    })
                    .collect::<Vec<_>>();
                note.push_str("Тот же текст есть в других оригиналах. Это отдельные документы/редакции, а не один объект; не объединяй их количества. Для проверки конкретного оригинала используй его идентификатор и хеш:\n");
                note.push_str(&json!({"same_text_origins":identities}).to_string());
                note.push('\n');
            }
        }
        let identity: serde_json::Map<_, _> = [
            "doc_id",
            "dataset_id",
            "source_sha256",
            "page",
            "sheet_name",
            "row",
        ]
        .into_iter()
        .filter(|key| !source[*key].is_null())
        .map(|key| (key.to_owned(), source[key].clone()))
        .collect();
        let mut block = json!({"schema":"les.evidence-block.v1","source_index":source["index"],"text":format!(
            "{}\n{}\n{note}{text}",
            source["header"].as_str().unwrap_or(""),
            Value::Object(identity)
        )});
        if source["context_unit"].is_string() {
            block["context_unit"] = source["context_unit"].clone();
            let mut common: serde_json::Map<_, _> = ["doc_id", "dataset_id", "source_sha256"]
                .into_iter()
                .map(|key| (key.to_owned(), source[key].clone()))
                .collect();
            let name = source["doc_name"].as_str().unwrap_or("");
            common.insert(
                "doc_name".into(),
                json!(name.rsplit(['/', '\\']).next().unwrap_or(name)),
            );
            block["compact_header"] = json!(format!(
                "[Источник {}{}]",
                source["index"],
                source["label"]
                    .as_str()
                    .unwrap_or("")
                    .strip_prefix(name)
                    .unwrap_or("")
            ));
            let location: serde_json::Map<_, _> = ["page", "sheet_name", "row"]
                .into_iter()
                .filter(|key| !source[*key].is_null())
                .map(|key| (key.to_owned(), source[key].clone()))
                .collect();
            block["document_identity"] = json!(common);
            block["text"] = json!(format!(
                "{}\n{}\n{note}{text}",
                source["header"].as_str().unwrap_or(""),
                Value::Object(location)
            ));
        }
        // Byte offset into the retained original, not a duplicate of the quote.
        block["body_offset"] = json!(block["text"].as_str().unwrap().len() - text.len());
        if source["table_projection_warning"].is_string() {
            block["table_geometry_unverified"] = json!(true);
        }
        if canonical["schema"] == "les.document-structure.v1" {
            block["canonical"] = canonical.clone();
        }
        if source["sheet_name"].is_string() && source["row"].is_u64() {
            block["record_kind"] = json!("spreadsheet_row");
        }
        block
    }
    pub(super) fn terminal_block(block: &mut Value) {
        // The terminal model needs the citation label and body. Machine
        // identities remain in the source map for verification and receipts.
        let Some(text) = block["text"].as_str() else {
            return;
        };
        let Some(offset) = block["body_offset"].as_u64().map(|n| n as usize) else {
            return;
        };
        let Some((header, rest)) = text.split_once('\n') else {
            return;
        };
        let Some((identity, body)) = rest.split_once('\n') else {
            return;
        };
        if !header.starts_with("[Источник ") || header.len() + identity.len() + 2 > offset {
            return;
        }
        let offset = offset - identity.len() - 1;
        block["text"] = json!(format!("{header}\n{body}"));
        block["body_offset"] = json!(offset);
        block.as_object_mut().unwrap().remove("document_identity");
    }
    fn key(source: &Value) -> Value {
        if source["workbook_group"].is_object()
            && source["sheet_name"].is_string()
            && source["row"].is_u64()
        {
            return json!([
                source["doc_id"],
                source["source_sha256"],
                source["sheet_name"],
                source["row"]
            ]);
        }
        json!([
            source["doc_id"],
            source["source_sha256"],
            source["page"],
            source["qdrant_point_id"],
            source["offset"],
            source["part_sha256"],
            source["part_page"],
            source["view_sha256"],
            source["ordinal"],
            source["pdf_excerpt_line"],
            source["sheet_name"],
            source["row"]
        ])
    }
    pub(super) fn add(&mut self, result: &Value) {
        let result = super::protocol::effective(result);
        if !result["result"]["evidence_packet"].is_null() {
            if let Err(code) = super::tools::sources::packet::decode(result) {
                self.read_failures.push(json!({"code":code}));
                self.delivered.clear();
                return;
            }
        }
        for failure in result["result"]["read_failures"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if !self.read_failures.contains(failure) {
                self.read_failures.push(failure.clone());
            }
        }
        let sources = &self.sources;
        let current = &self.blocks;
        let retained = &self.retained;
        let initial = &self.initial;
        let assembled = self.pages.normalize(result, |key| {
            let source = sources.iter().find(|s| s["native_identity"] == *key)?;
            let block = current
                .iter()
                .chain(retained.iter().map(|(_, b)| b))
                .chain(initial.iter().map(|(_, b)| b))
                .find(|b| b["source_index"] == source["index"])?;
            Self::text(block)
                .get(usize::try_from(block["body_offset"].as_u64()?).ok()?..)
                .map(str::to_owned)
        });
        let result = assembled.as_ref().unwrap_or(result);
        // The current read is mandatory. Earlier reads and search candidates
        // remain available as whole optional blocks, with durable originals.
        self.add_passages(
            result,
            super::protocol::effective(result)["tool"] == "search_sources",
        );
        let result = super::protocol::effective(result);
        let reader = matches!(
            result["tool"].as_str(),
            Some("read_source" | "read_pdf_source" | "read_excel_source")
        ) || (result["tool"] == "document_review" && result["operation"] == "read");
        let receipt = &result["result"]["read_receipt"];
        let indexed_read = result["operation"] == "indexed_sections"
            && receipt["schema"] == "les.indexed-read.v1"
            && !receipt["index_revision"].is_null()
            && receipt["index_revision"] == result["result"]["index_revision"]
            && receipt["point_ids"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
        if result["status"] != "ok"
            || !(reader || (result["tool"] == "search_sources" && indexed_read))
        {
            return;
        }
        let indexes: Vec<_> = Self::passages(result)
            .iter()
            .filter(|p| p["text"].as_str().is_some_and(|s| !s.trim().is_empty()))
            .filter(|p| {
                result["tool"] != "search_sources"
                    || receipt["point_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&p["source"]["qdrant_point_id"]))
            })
            .filter_map(|p| {
                self.sources
                    .iter()
                    .find(|s| s["native_identity"] == Self::key(&p["source"]))
            })
            .filter_map(|s| s["index"].as_u64())
            .collect();
        if !indexes.is_empty() {
            self.delivered.clear();
        }
        if reader && result["tool"] != "document_review" {
            self.explicit_reads.extend(indexes.iter().copied());
        }
        if result["tool"] == "search_sources" && result["result"]["pull_search"] == true {
            self.search_heads.extend(indexes.first().copied());
        }
        self.verified_reads.extend(indexes);
    }
    pub(super) fn answer_blocks(&self, min_documents: usize) -> Option<Vec<Value>> {
        if self.explicit_reads.is_empty() && self.search_heads.is_empty() {
            return None;
        }
        let mut documents = std::collections::BTreeSet::new();
        let (mut first, rest): (Vec<_>, Vec<_>) = self
            .required_blocks()
            .into_iter()
            .chain(self.optional_blocks())
            .partition(|block| {
                let Some(index) = block["source_index"].as_u64() else {
                    return false;
                };
                let head = self.search_heads.contains(&index);
                if !head && !self.explicit_reads.contains(&index) {
                    return false;
                }
                let source = &self.sources[index as usize - 1];
                let doc = source["doc_id"].as_str();
                let dataset = source["dataset_id"].as_str();
                documents.insert((dataset, doc)) || head
            });
        if documents.len() < min_documents && self.search_heads.len() < min_documents {
            return None;
        }
        first.extend(rest);
        Some(first)
    }
    pub(super) fn prioritize_workbook_groups(&self, question: &str, blocks: &mut [Value]) {
        use les_core::tables::group_identifiers as identifiers;
        let wanted: std::collections::HashSet<_> = identifiers(question).collect();
        if wanted.is_empty() {
            return;
        }
        blocks.sort_by_cached_key(|block| {
            let group = block["source_index"]
                .as_u64()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| self.sources.get(i as usize))
                .and_then(|source| source["workbook_group"]["title"].as_str());
            !group.is_some_and(|name| identifiers(name).any(|id| wanted.contains(&id)))
        });
    }
    /// A terminal selection for independently named files can use only the
    /// strongest matching reading units from each file. Units which supply
    /// different query terms or tie stay together. The caller can retry with
    /// the complete evidence when the selection leaves a part unanswered.
    pub(super) fn citation_focus(
        &self,
        question: &str,
        selected_files: &[Value],
        blocks: &[Value],
    ) -> Option<Vec<Value>> {
        if selected_files.len() < 2 || !self.explicit_reads.is_empty() || blocks.len() > 240 {
            return None;
        }
        let mut retained = std::collections::BTreeSet::new();
        for file in selected_files {
            let name = file.as_str()?;
            let query = super::inspection::read_question(question, Some(name), selected_files);
            if query == question {
                return None;
            }
            let basename = name.rsplit(['/', '\\']).next().unwrap_or(name);
            let title = basename.rsplit_once('.').map_or(basename, |(stem, _)| stem);
            let candidates: Vec<_> = blocks
                .iter()
                .enumerate()
                .filter_map(|(i, block)| {
                    let index = block["source_index"].as_u64()?.checked_sub(1)? as usize;
                    if self.sources.get(index)?["doc_name"] != *file {
                        return None;
                    }
                    let body = Self::text(block).get(block["body_offset"].as_u64()? as usize..)?;
                    Some((i, body))
                })
                .collect();
            if candidates.is_empty() {
                return None;
            }
            let texts: Vec<_> = [query, title]
                .into_iter()
                .chain(candidates.iter().map(|(_, body)| *body))
                .map(str::to_owned)
                .collect();
            let tokens = les_core::lexical::reading(&texts).ok()?;
            let terms = tokens["documents"][0]["terms"].as_object()?;
            let terms: Vec<_> = terms
                .keys()
                .filter(|term| tokens["documents"][1]["terms"].get(*term).is_none())
                .collect();
            let matches: Vec<_> = candidates
                .iter()
                .enumerate()
                .map(|(i, (index, _))| {
                    let available = &tokens["documents"][i + 2]["terms"];
                    (
                        *index,
                        terms
                            .iter()
                            .enumerate()
                            .filter_map(|(term, key)| available.get(*key).is_some().then_some(term))
                            .collect::<std::collections::BTreeSet<_>>(),
                    )
                })
                .collect();
            if matches.iter().all(|(_, terms)| terms.is_empty()) {
                return None;
            }
            for (index, terms) in &matches {
                if !terms.is_empty()
                    && !matches
                        .iter()
                        .any(|(_, other)| other.len() > terms.len() && terms.is_subset(other))
                {
                    retained.insert(*index);
                }
            }
        }
        (retained.len() < blocks.len()).then(|| {
            retained
                .into_iter()
                .map(|index| {
                    let mut block = blocks[index].clone();
                    // Keep the proof ID and body. A focused unit belongs on its own,
                    // even when it was originally included with a full document.
                    block.as_object_mut().unwrap().remove("context_unit");
                    block
                })
                .collect()
        })
    }

    pub(super) fn optional_blocks(&self) -> Vec<Value> {
        // Reading another source adds evidence. It does not silently replace
        // documents already read by native prefetch or an earlier tool call.
        // Admission and the shared context budget remain the only filters.
        self.retained
            .iter()
            .rev()
            .chain(self.initial.iter())
            .filter(|(index, _)| !self.required || self.verified_reads.contains(&(*index as u64)))
            .map(|(_, block)| block.clone())
            .collect()
    }
    pub(super) fn required_blocks(&self) -> Vec<Value> {
        self.blocks
            .iter()
            .filter(|block| {
                !self.required
                    || block["source_index"]
                        .as_u64()
                        .is_some_and(|i| self.verified_reads.contains(&i))
            })
            .cloned()
            .collect()
    }
    fn add_passages(&mut self, result: &Value, initial: bool) {
        let result = super::protocol::effective(result);
        if result["status"] != "ok" {
            return;
        }
        let has_text = result["result"]["text"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            || result["result"]["passages"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|p| p["text"].as_str().is_some_and(|s| !s.is_empty()))
            });
        if !initial && result["tool"] != "document_review" && has_text {
            // Retained batches are consumed newest first. Reverse the insert
            // order so the most relevant units within each read stay first.
            for block in self.blocks.drain(..).rev() {
                if let Some(source) = self.sources.iter().find(|s| {
                    Self::text(&block)
                        .starts_with(&format!("{}\n", s["header"].as_str().unwrap_or("")))
                }) {
                    let index = source["index"].as_u64().unwrap() as usize;
                    self.retained.retain(|(i, _)| *i != index);
                    self.retained.push((index, block));
                }
            }
        }
        if result["tool"] == "read_pdf_source"
            && result["result"]["complete"] == true
            && result["result"]["offset"] == 0
        {
            let page = &result["sources"][0];
            let replaced = self
                .sources
                .iter()
                .filter(|source| {
                    ["doc_id", "source_sha256", "page"]
                        .iter()
                        .all(|field| source[*field] == page[*field])
                })
                .collect::<Vec<_>>();
            self.initial
                .retain(|(index, _)| !replaced.iter().any(|source| source["index"] == *index));
            self.retained
                .retain(|(index, _)| !replaced.iter().any(|source| source["index"] == *index));
            self.blocks.retain(|block| {
                !replaced.iter().any(|source| {
                    Self::text(block)
                        .starts_with(&format!("{}\n", source["header"].as_str().unwrap_or("")))
                })
            });
        }
        let passages = Self::passages(result);
        for passage in passages {
            let mut source = passage["source"].clone();
            if source["evidence_admissible"] == false {
                continue;
            }
            let Some(text) = passage["text"].as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            if !source.is_object() {
                continue;
            }
            let key = Self::key(&source);
            if let Some(index) = self
                .sources
                .iter()
                .position(|s| s["native_identity"] == key)
            {
                if source["prepared_evidence_origin"] == true {
                    for field in ["index", "native_identity", "label", "header"] {
                        source[field] = self.sources[index][field].clone();
                    }
                    Self::page_metadata(&mut source, result);
                    let block = Self::block(&source, text, &passage["canonical"]);
                    self.sources[index] = source;
                    self.initial.retain(|(i, _)| *i != index + 1);
                    self.retained.retain(|(i, _)| *i != index + 1);
                    self.blocks.retain(|b| b["source_index"] != index + 1);
                    if initial {
                        self.initial.push((index + 1, block));
                    } else {
                        self.blocks.push(block);
                    }
                    continue;
                }
                Self::page_metadata(&mut self.sources[index], result);
                if !initial && result["tool"] != "document_review" {
                    self.initial.retain(|(i, _)| *i != index + 1);
                    self.retained.retain(|(i, _)| *i != index + 1);
                    if !self.blocks.iter().any(|b| {
                        Self::text(b).starts_with(&format!(
                            "{}\n",
                            self.sources[index]["header"].as_str().unwrap_or("")
                        ))
                    }) {
                        self.blocks.push(Self::block(
                            &self.sources[index],
                            text,
                            &passage["canonical"],
                        ));
                    }
                }
                continue;
            }
            let index = self.sources.len() + 1;
            source["index"] = json!(index);
            source["native_identity"] = key;
            let name = source["doc_name"].as_str().unwrap_or("Документ");
            let label = source["page"]
                .as_u64()
                .map(|p| format!("{name} · стр. {p}"))
                .or_else(|| {
                    source["sheet_name"].as_str().map(|sheet| {
                        let mut label = format!("{name} · лист {sheet}");
                        if let Some(row) = source["row"].as_u64() {
                            label.push_str(&format!(" · строка {row}"));
                        }
                        label
                    })
                })
                .unwrap_or_else(|| name.into());
            let header_label = if source["doc_id"].is_string() {
                format!(
                    "{}{}",
                    name.rsplit(['/', '\\']).next().unwrap_or(name),
                    label.strip_prefix(name).unwrap_or("")
                )
            } else {
                label.clone()
            };
            source["label"] = json!(label);
            source["header"] = json!(format!("[Источник {index} | {header_label}]"));
            Self::page_metadata(&mut source, result);
            if result["tool"] != "document_review" {
                let block = Self::block(&source, text, &passage["canonical"]);
                if initial {
                    self.initial.push((index, block));
                } else {
                    self.blocks.push(block);
                }
            }
            self.sources.push(source);
        }
    }
    fn passages(result: &Value) -> Vec<Value> {
        if !result["result"]["evidence_packet"].is_null() {
            return super::tools::sources::packet::decode(result)
                .map(|expanded| Self::passages(&expanded))
                .unwrap_or_default();
        }
        result["result"]["passages"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                result["result"]["text"]
                    .as_str()
                    .map(|s| vec![json!({"text":s,"source":result["sources"][0]})])
                    .unwrap_or_default()
            })
    }
    fn page_metadata(source: &mut Value, result: &Value) {
        if result["tool"] != "read_pdf_source"
            || result["sources"][0]["evidence_kind"] != "physical_pdf_page"
        {
            return;
        }
        source["evidence_kind"] = json!("physical_pdf_page");
        source["offset"] = result["sources"][0]["offset"].clone();
        source["next_offset"] = result["sources"][0]["next_offset"].clone();
        let body = &result["result"];
        source["page_text_complete"] = json!(
            body["selection"] == "page_range" && body["complete"] == true && body["offset"] == 0
        );
        source["assembled_part_count"] = json!(body["assembled_part_count"].as_u64().unwrap_or(1));
    }
    fn reference(&self, source: &Value) -> Option<Value> {
        let key = Self::key(source);
        let key = self.pages.canonical(source).unwrap_or(&key);
        self.sources
            .iter()
            .find(|s| s["native_identity"] == *key)
            .map(|s| json!({"index":s["index"],"label":format!("[Источник {}]",s["index"]),
                "doc_id":s["doc_id"],"doc_name":s["doc_name"].as_str().unwrap_or("").rsplit(['/', '\\']).next().unwrap_or(""),
                "source_sha256":s["source_sha256"],"page":s["page"]}))
    }
    pub(super) fn review_context(&self, view: &Value) -> Result<Value> {
        Ok(json!(format!(
            "{}\n{}",
            self.review_header(view),
            les_core::tool_execution::canonical_json(view)?
        )))
    }
    pub(super) fn review_header(&self, view: &Value) -> String {
        let read = &view["current_read"];
        self.sources
            .iter()
            .find(|s| {
                s["qdrant_point_id"] == read["fragment_id"]
                    && s["offset"] == read["offset"]
                    && s["dataset_id"] == read["selector"]["dataset_id"]
                    && s["doc_name"] == read["selector"]["file_name"]
                    && s["source_sha256"] == read["source_sha256"]
            })
            .and_then(|s| s["header"].as_str())
            .unwrap_or("")
            .to_owned()
    }
    pub(super) fn projection(&self, payload: &Value, context_chars: usize) -> Value {
        let result = super::protocol::effective(payload);
        if self.needs_read() || result["result"]["documents"].is_array() {
            return super::discovery::project(payload, context_chars);
        }
        let mut value = match super::tools::sources::packet::decode(payload) {
            Ok(value) => value,
            Err(code) => return json!({"status":"error","result":{"code":code}}),
        };
        if let Some(envelope) = value.as_object_mut() {
            envelope.remove("_pull_evidence");
            envelope.remove("_pull_workbook_evidence");
        }
        if let Some(result) = value["result"].as_object_mut() {
            result.remove("read_receipt");
        }
        // The private journal owns normalized source requests. The provider's
        // call already carries those arguments; never send another copy.
        if [
            "search_sources",
            "read_source",
            "read_pdf_source",
            "read_excel_source",
        ]
        .iter()
        .any(|name| value["tool"] == *name)
        {
            value.as_object_mut().unwrap().remove("inputs");
        }
        if value["tool"]
            .as_str()
            .is_some_and(|v| v.starts_with("use_extensions_"))
            && value["operation"] == "call"
        {
            value["result"] = self.projection(&value["result"], context_chars);
        }
        if let Some(rows) = value["result"]["passages"].as_array_mut() {
            for row in rows {
                if let Some(reference) = self.reference(&row["source"]) {
                    row.as_object_mut().unwrap().remove("text");
                    row.as_object_mut().unwrap().remove("canonical");
                    // The admitted evidence block owns canonical identity and
                    // location. A tool ledger needs the citation pointer, not
                    // another copy of paths, hashes and nested source_ref.
                    row.as_object_mut().unwrap().remove("source_ref");
                    row["source"] = json!({"index":reference["index"],"label":reference["label"],"doc_id":reference["doc_id"]});
                }
            }
            // Every passage already carries the exact reference. The full
            // envelope and text remain unchanged in the durable tool receipt.
            value.as_object_mut().unwrap().remove("sources");
        }
        if value["result"]["text"].is_string() {
            if let Some(reference) = self.reference(&value["sources"][0]) {
                value["result"]["text"] = reference["label"].clone();
            }
        }
        if value["sources"].is_array() {
            value["sources"] = json!(value["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| self.reference(s).unwrap_or_else(|| s.clone()))
                .collect::<Vec<_>>());
        }
        if !value["execution"].is_null() {
            value["execution"] =
                json!({"status":value["execution"]["status"],"code":value["execution"]["code"]});
        }
        if value["result"]["candidate_documents"].is_array() {
            let cards = super::inspection::candidates(
                &value["result"]["candidate_documents"],
                (context_chars / 4).clamp(256, 1200),
            );
            value["result"]["candidate_documents"] = cards["documents"].clone();
            value["result"]["candidate_document_count"] = cards["total"].clone();
            value["result"]["discovery_evidence_admissible"] = json!(false);
        }
        value
    }
}
