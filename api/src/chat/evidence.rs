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
        assert!(!evidence.document_omitted(&json!([])));
        result["tool"] = json!("read_source");
        evidence.add(&result);
        assert!(evidence.document_omitted(&json!([{"index":1}])));
        assert!(!evidence.document_omitted(&json!([{"index":1},{"index":2}])));
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
    fn search_is_optional_but_read_promotes_text_and_keeps_exact_source_identity() {
        let source = json!({"doc_id":"catalog-doc","dataset_id":"ds","doc_name":format!("{}/Source.pdf","long-folder/".repeat(30)),"source_sha256":"a".repeat(64),"page":3,"qdrant_point_id":"point"});
        let mut envelope = json!({"status":"ok","tool":"search_sources","result":{"passages":[{"text":"Exact proof 17.35 kW","source":source}]},"sources":[source]});
        let mut evidence = Evidence::default();
        evidence.add(&envelope);
        assert!(evidence.blocks.is_empty());
        assert_eq!(evidence.optional_blocks().len(), 1);
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
        let text = Evidence::text(&evidence.blocks[0]);
        assert!(
            text.contains("catalog-doc")
                && text.contains(&"a".repeat(64))
                && text.contains("Exact proof 17.35 kW")
        );
    }
}
impl Evidence {
    pub(super) fn new(required: bool) -> Self {
        Self {
            required,
            ..Self::default()
        }
    }
    pub(super) fn requires_sources(&self) -> bool {
        self.required
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
    pub(super) fn document_omitted(&self, visible: &Value) -> bool {
        self.sources.iter().any(|source| {
            source["context_unit"].is_string()
                && source["document_fragment_count"]
                    .as_u64()
                    .is_some_and(|n| n > 1)
                && source["index"]
                    .as_u64()
                    .is_some_and(|i| self.verified_reads.contains(&i))
                && !visible
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|s| s["index"] == source["index"])
        })
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
    fn block(source: &Value, text: &str) -> Value {
        let mut note = if source["ocr"] == true {
            "Текст получен OCR: возможны ошибки знаков, чисел и структуры таблицы. Для точных выводов проверь оригинал.\n"
        } else {
            ""
        }.to_owned();
        if source["retrieval_incomplete"] == true {
            note.push_str("Часть найденных оригиналов не дочитана из-за ошибки. Обзор неполный: отвечай только по прочитанному, не утверждай, что в непрочитанных файлах нет ответа.\n");
        }
        if source["visual_content"] == true {
            note.push_str("Документ содержит рисунки/вложения. Текстовая выдержка не подтверждает геометрию, графики и детали изображений. Оригиналы доступны через read_source view=parts; OCR не гарантирует чтение всех надписей.\n");
        }
        if let Some(count) = source["unread_visual_count"].as_u64().filter(|n| *n > 0) {
            note.push_str(&format!("Объектов без распознанного текста: {count}. Их содержимое не подтверждено этой выдержкой.\n"));
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
        if source["sheet_name"].is_string() && source["row"].is_u64() {
            block["record_kind"] = json!("spreadsheet_row");
        }
        block
    }
    fn key(source: &Value) -> Value {
        json!([
            source["doc_id"],
            source["source_sha256"],
            source["page"],
            source["qdrant_point_id"],
            source["offset"],
            source["part_sha256"],
            source["part_page"],
            source["view_sha256"],
            source["ordinal"]
        ])
    }
    pub(super) fn add(&mut self, result: &Value) {
        let result = super::protocol::effective(result);
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
        self.verified_reads.extend(indexes);
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
            for block in self.blocks.drain(..) {
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
                        self.blocks.push(Self::block(&self.sources[index], text));
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
                let block = Self::block(&source, text);
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
        let mut value = payload.clone();
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
