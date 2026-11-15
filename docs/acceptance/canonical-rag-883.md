# Build 883: canonical document structure

Status: implementation and isolated acceptance in progress, in draft PR #8.
Not installed, not merged and not a release. Working data and model assignments
are unchanged.

Scope: preserve the Docling structure through indexing, original reading and
evidence assembly; remove repeated Markdown structure detection for Word and
semantic selection before reading. BGE remains assigned; reranking stays off.

Required gates:

- Original Word corpus: numeric values, column association, literal operators,
  checked OCR/embedded content, source hashes and nonempty publication.
- Tokenized search chunks: bounded real BGE inputs, inherited headings and
  table headers, parent references and reading-unit checksums.
- Frozen MCP questions across both complete dataset scopes, without a file
  filter or answer-model calls: context delivery, physical citations and
  revision checks. Expected sources are used only after retrieval for scoring.
- Peak process-tree memory and minimum available physical memory; serial
  processing, no additional production index or model deployment.
- Targeted regressions, core/API tests, `make verify`, `make test`, publication
  checks and CI before merge. No test expectation is weakened to accept loss.

Earlier build 882 results are documented separately in
[structured-rag-882.md](structured-rag-882.md); they are not evidence for this
implementation. In particular, its failed CI and context coverage do not
become a passing build 883 acceptance by changing the parser representation.

Completed source gates for the initial 883 candidate:

- Core: 164 tests passed.
- Real native parser pipeline: 26 tests passed, including literal Markdown
  delimiters under four styles, merged mathematics, nested spans, OCR cell
  boundaries and physical PDF paging.
- `make verify`: passed, 2,222 tests collected. This is collection, not execution
  of the complete behavioral suite.
- API contract: 269 method/path pairs unchanged. Publication check passed.

Initial 883 isolated corpus conversion completed for all 78 DOCX originals,
with no conversion errors and unchanged source SHA-256. An independent OOXML
oracle matched 1,055 numeric rows in 34 explicitly bound tables, preserving
columns and order. This does not cover every table or validate diagram meaning.
The oracle reads explicit OOXML nonbreaking hyphens, tabs and breaks; ignoring
those elements would incorrectly concatenate range endpoints.

Peak native process-tree working set was 0.96 GiB (private bytes: 0.91 GiB),
with at least 11.8 GiB available system memory during this serial conversion
run. These are measurements of the initial debug candidate, not a performance
comparison or a bound for the persistent MCP service plus embedding model.

OCR limitations are unchanged across the same 78 original hashes: 270 visual
objects in 32 documents lack verified text. Original special parts are retained;
conversion success does not establish full visual extraction.

Follow-up source regressions passed: core 168 tests, API with Edge 111 tests.
These include native side-story table ownership, literal search identifiers,
typed-unit sanitation, canonical heading IDs and nonduplicated tool history.
The full behavioral suite, frozen MCP comparison and new complete index
acceptance remain pending. The initial head's CI failed on an incomplete test
fixture initializer; that initializer is corrected in the follow-up.
