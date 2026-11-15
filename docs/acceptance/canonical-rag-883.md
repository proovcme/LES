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

Token accounting separates the configured text budget, including metadata
prefixes, from provider-added control tokens. The native counter excludes those
control tokens: a local BGE protocol probe measured 7 content tokens versus 9
reported by Ollama. A 512-token text chunk must not be reported as a total
512-token provider input. The new-index point check pins the tokenizer hash,
checks every prefixed text chunk and separately probes the largest input with
provider truncation disabled against the provider's reported context limit.

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

The subsequent complete behavioral run found eight failures (2,195 passed,
19 skipped). They block merge. Three causes are being corrected: prose addresses
depended on a table being in the same fragment; read planning discarded hydrated
continuations and could shrink the reported document extent; typed cell arrays
exceeded the old flat-text item allowance. The corrected candidate must repeat
these real API/cursor regressions and the full suite before corpus acceptance.

The first targeted repeat passed the mixed-prose selection and 500-row Word
cursor scenarios. Six answer-window variants still failed: the retry incorrectly
required a whole-document completeness marker rather than checking omitted
verified reads. That delivery check is now separated from completeness claims;
the targeted repeat now passes all eight previously failing scenarios (39.4 s).
Core tests passed 168, API with Edge passed 114. These checks do not establish
retrieval quality; the frozen corpus metrics are still pending.

The complete behavioral repeat then passed: 2,203 passed, 19 skipped, 48 warnings,
18 subtests passed in 1,133.26 seconds. No failures remain in that suite. Peak
owned test-process working set was 0.95 GiB; minimum available system memory
was 10.41 GiB. These measurements describe the test tree, not the persistent
retrieval service or BGE. Optimized-copy MCP metrics and new-index acceptance
remain pending, and this candidate is still not installed or merged.

## Frozen100 comparison on the existing copied index

The first optimized 883 reader run completed all 100 unchanged MCP questions,
with the same two dataset scopes and 12,000-character context limit. No expected
file/page was supplied to retrieval; no answer model was called.

| Measure | Previous candidate | Initial canonical reader |
|---|---:|---:|
| Needed-source delivery, 90 positive cases | 58.70% | 59.07% |
| Reference-text 5-gram coverage, 69 cases | 43.33% | 47.83% |
| Observed search latency p50 | 5.06 s | 7.14 s |
| Tool-level search errors | 0 | 0 |

These are context-delivery measures, not answer accuracy. Latency includes the
run's original-reading/cache conditions and is not an isolated speed comparison.
The candidate fails acceptance: one previously delivered source was lost.
Verbose read-failure metadata grew from about 3.5k to 8.7k characters in that
case and competed with proof bodies under the unchanged wire limit.

Independent post-retrieval MCP checks verified the bytes of 155 cited originals
and completed physical-page identity/cursor/digest checks for 239 PDF pages.
There were 22 failed read attempts across 12 distinct pages, all reporting
`PDF_TABLE_SOURCE_MISMATCH`. The optional table projection currently invalidates
the independently verified original text; that ownership boundary is being fixed.
Neither those failures nor the lost source are counted as a passing result.

Peak owned retrieval-service process-tree working set was 2.48 GiB; minimum
available system RAM was 8.68 GiB. The service-tree peak excludes the separately
running embedding provider. The working installation was unchanged.

Unaccepted follow-up: compact only redundant failure locator/path data while
retaining every cause and retry identity; reject unproved table enrichment while
preserving the revision-checked original, visibly marking uncertain geometry and
disabling row arithmetic for it. Targeted, corpus and CI acceptance must repeat.

The follow-up source gates passed core 169 / API with Edge 117 tests. The new
real-PDF regression injects an unproved numeric value into the auxiliary table
cache: indexing and physical reading retain the unchanged original value, a
page warning, nonempty nodes and the original SHA. A test-fixture access error
(`payload.text` instead of the node's text) was corrected without weakening
those assertions. The other 64 parser/RAG scenarios passed in that run; the
corrected PDF scenario passed separately. A complete targeted repeat is pending.
The source-reader component passed four tests plus typecheck/build.

The head's GitHub run also failed on concurrent settings publication
(`SECRET_STORE_UNAVAILABLE`); its other 2,199 cases passed, 22 were skipped.
Eighty ordinary local parallel cycles did not reproduce it. A Windows reader
that temporarily denies rename reproduced the same failure while preserving
the old file. The follow-up retries only transient Windows publication failures,
retains writer locking, permissions and durability, checks external changes
before each retry and logs the failing stage without values. Its forced-lock
regression and 80 additional parallel FFI cycles passed. This is local evidence;
the failed GitHub gate must still be repeated successfully.

The complete targeted follow-up then passed all 70 parser, retrieval-mode and
native model-runtime cases in 284.90 seconds. `make verify` passed with 2,223
cases collected; publication checks passed. Full CI and both copied-index corpus
comparisons remain merge blockers. No working installation has been changed.

## Follow-up: complete unchanged Frozen100 repeat

The optimized follow-up completed the same 100 MCP questions on the existing
copied index, with the same dataset scopes, 12,000-character context and frozen
scorer. Expected files and pages were not supplied to retrieval. No answer model
was called.

| Measure | Previous candidate | Follow-up reader |
|---|---:|---:|
| Needed-source delivery, 90 positive cases | 58.70% | 56.85% |
| Reference-text 5-gram coverage, 69 cases | 43.33% | 46.38% |
| Expected physical page hit, 12 cases | 91.67% | 83.33% |
| Expected-page coverage, 12 cases | 87.50% | 79.17% |
| Expected Excel row hit, 2 cases | 100% | 100% |
| Observed search latency p50 | 5.06 s | 6.61 s |
| Tool-level search errors | 0 | 0 |

This is not an accepted quality improvement. Three previously delivered expected
sources were lost, while two cases gained source coverage. A separate diagnostic,
performed only after retrieval, found the complete reference quote in another
returned document in two of those three cases; one of those cases previously
returned the expected document without its reference quote. This does not prove
equivalent editions or change the frozen scores. The remaining lost conclusion
is unresolved. Both original and alternative sources must remain inspectable;
an aggregate percentage cannot substitute for case-level acceptance.

Independent checks verified all bytes of 155 cited originals and completed
identity, page, continuation and digest checks for 270 physical PDF pages, with
zero read errors. Twenty-eight pages carried the explicit unverified-table-
geometry warning. Their text availability does not certify cell associations or
permit inferred row arithmetic. Peak owned service-tree working set was 2.31 GiB;
minimum available system memory was 8.83 GiB, excluding the external embedding
provider from the process-tree measurement. Copied source-catalog rows and the
question-file hash remained unchanged.

These results exercise a new reader over the old search fragments. A separate
complete rebuild of all 638 registered originals into the new canonical v4
representation has started, using an independent Edge store and serial batches.
That index and its frozen-query comparison are not yet accepted. Current CI and
corpus regressions continue to block merge; the working installation remains 880.

Subsequent read-only diagnosis of the raw, unchanged scoped search found the
missing conclusion at fused ranks 60/61 and physical-scope positions 40/41.
The 32-scope read ceiling discarded it before original reading. The follow-up
source change makes every retrieved anchor a required read, with optional parent
pages using only remaining expansion capacity; it does not alter RRF, the
retrieval shortlist, answer budget or frozen scorer. A generic 64-source
regression also covers a reserved channel head with a late true rank. This
change is not yet validated on the corpus and does not retroactively improve
the measurements above.

The first full new-index run also exposed exhausted `WORKER_TIMEOUT` failures
in real PDF imports. One confirmed original has 195 pages; the optional whole-book
table job repeatedly exceeded its deadline before indexing could retain the
original text. Other documents continued processing. This is an unaccepted
ingestion failure, not an empty successful publication.

The subsequent source change separates this optional timeout from original
reading, retains page-specific uncertainty, reuses a source/revision-bound failed
attempt during the OCR pass and retries uncertain geometry through the physical
page reader. Revision, invalid-cache, OCR and memory guards are not waived.
It also allows an explicit folder indexing request to retry exhausted errors
without reindexing unchanged ready files or automatically looping on unchanged
watch failures. Native boundary regressions are added; the real failed imports,
full point proof and frozen MCP comparisons must still repeat. No new score or
passing ingestion claim is assigned to those source changes yet.

The same run found a separate `OCR_NO_TEXT` failure in a 318-page PDF, at page
308. Native text is available on hundreds of its pages; visual inspection of the
failed page showed a decorative blank form with serial/page markings. The index
converter now records such a page as unrecognized and retains readable pages
before and after it, with explicit partial-visual coverage. An entirely
unrecognized document and a strict standalone OCR request still fail. Other OCR
errors are not hidden. This boundary is covered by a three-page regression;
the real corpus repeat remains required. This is not a claim that all scans or
figures were successfully recognized.

## Current source and copy gates

The previous follow-up's complete GitHub run passed: 2,201 Python scenarios,
22 skips and 18 subtests, together with core 169, Edge API 117 and the UI checks.
This does not validate subsequent changes. The current Rust source separately
passed core 169 and Edge API 119 unit tests. Its optimized private API build
completed in 341.75 seconds with peak build-child working set 1.14 GiB and
minimum available system memory 10.25 GiB. Build memory is not runtime memory.
A full debug executable link was stopped at the 4 GiB reserve; the reserve was
not lowered and user applications were not closed.

The first targeted native repeat passed 109 cases, skipped one and failed one
assertion. Inspection of its saved real API response showed the selected prose
and exact quote correctly bound to source 3, while the assertion required a quote
on source 1. The corrected test follows the receipt's source index, checks its
quote and answer citation, and rejects attaching that quote to other sources.
It does not impose retrieval order or copy the quote across sources. A complete
targeted repeat and the unchanged 100-question MCP repeat are in progress.

The first new-generation import was deliberately interrupted after 630 ready
documents, four errors and four pending documents. Its original state, reasons
and job history are retained. The next step resumes the same private generation
and retries both whole registered roots through the public indexing API. Ready
documents must retain their hashes and chunk counts; originals remain read-only.
Neither this controlled interruption nor the pending repeat is claimed as a
successful publication-crash test. Full point checks, corpus measurements and
current CI still block merge. The working installation remains 880.

## Complete copied-generation comparison and follow-up

The corrected citation test passed in the complete targeted repeat: **110 passed,
one skipped** (the optional legacy Edge-import executable was not supplied).
The interrupted v4 import resumed the existing job with `INTAKE_INTERRUPTED` and
one recorded recovery, then explicit indexing requests retried both complete
registered roots. All **638 originals** now have nonempty indexed content.
The original 630 ready documents retain their hashes/chunk counts; all original
file hashes remain unchanged. This is controlled-interruption recovery evidence,
not an unannounced publication-crash claim.

All **88,175 points / 81,897 content inputs** passed source/hash/token checks;
there are **18,341 Word reading anchors / 1,541 table anchors**. Maximum prefixed
content is 512 tokens; the independent untruncated provider probe reported 514
including control tokens, below its 8,192-token context. An inspection-script
field-name mistake initially miscounted uncertain PDF geometry as zero. That
number was rejected, the initial proof retained, and the complete point check
repeated with the actual field: **1,821 page identities carry uncertainty**.
No perfect-layout claim follows from availability of their text.

Both complete MCP repeats used the frozen 100 questions, both whole dataset
scopes, the unchanged 12,000-character text budget and no answer model/file filter.
The rebuilt catalog has new document IDs, so comparisons below use the same
original-byte hashes; this agrees with the older primary-ID score.

| Measure | Previous accepted candidate | Current reader, old copy | Current reader, v4 copy |
|---|---:|---:|---:|
| Needed-source delivery, 90 cases | 58.70% | 56.48% | 64.07% |
| Reference-text coverage, 69 cases | 43.33% | 44.60% | 51.84% |
| Expected-page hit, 12 cases | 91.67% | 91.67% | 91.67% |
| Expected-page coverage, 12 cases | 87.50% | 87.50% | 87.50% |
| Excel row hit, 2 cases | 100% | 100% | 100% |
| Observed search latency p50 | 5.06 s | 8.61 s | 8.93 s |
| Search-tool errors | 0 | 0 | 0 |

The v4 change is +5.37 percentage points for sources and +8.51 for reference
text, but case regressions remain. Paired question bootstrap intervals (20,000
draws) are respectively [-2.22, 12.96] and [-2.75, 20.11] points. This is not a
statistically established or sufficient general improvement, nor answer accuracy.
The old-copy run overlapped targeted native tests initially; timings are observed
end-to-end measurements, not a controlled attribution of slowdown to one stage.

The old-copy repeat checked 156 originals / 266 physical pages; v4 checked
164 / 202, with zero byte/identity/cursor/digest errors among returned citations.
Both cohorts retain pinned, scoped sources, no exact same-source passage
duplicates, and at most the requested text budget. This does not mean every
candidate original was read: old-copy failures include seven large-page atomic
overflows; v4 has eleven such failures and three worker timeouts. These remain
visible and cannot support absence claims. V4 owned service peak is 2.51 GiB,
minimum available system RAM 7.22 GiB; the external BGE process is not included
in the service-tree peak. No resource reserve was lowered.

Subsequent source changes fold exact repeated identity metadata before wire
trimming, materialize long original pages through the existing structural reader,
and fix the final Word-revision check's caller (previously gated on PDF presence).
They preserve the frozen questions and all budgets. Native regressions include
a real long multi-column PDF retrieved by question alone, Unicode source positions,
and replacing a Word original during completion. The UI keeps the digest and
opens the original PDF position. These changes are not included in the table
above and must pass a new native/corpus repeat. Merge remains blocked; production
installation/models/index are unchanged.

The first follow-up API unit run passed 122 cases and failed the long-table
header assertion. Its newly materialized page included a standalone title in
addition to the same title carried by the table. The fix reuses the existing
core heading projection with an explicit context purpose, removes whitespace-
only evidence units and leaves the default raw reader's stream/hash unchanged.
The header/row assertions are retained. The preceding full CI (`abf5eac`) passed
core 169, API 119, UI 54 and 2,205 Python cases plus 18 subtests, with 22 skips;
it does not cover this follow-up. Verification CI now uses the same optimized
test profile as the copy stand (opt 2, LTO off, 16 codegen units), with dependency
debug symbols disabled. It retains the complete behavior suite; the separate
installer workflow keeps its production profile. This addresses compiler cost
in feedback, not measured RAG latency.

The follow-up optimized build passed **170 core / 123 API cases**; the complete
frontend suite passed **55 cases in 14 files**. The native service repeat passed
111 cases, skipped the optional legacy importer, and exposed an incorrect new
test expectation: original replacement correctly returned the source-tool
contract's **409 / PDF_REVISION_CHANGED**, while the test expected a generic
503. All preceding assertions had verified the late original-page text, unit
digest and pinned Unicode-position read. The corrected case passed separately;
the failed first log is retained. This composes 112 passing native cases and
one skip, not a claim that the first run was green. The Word replacement during
completion also passed with the cause recorded in run history.

The same 100-question comparisons are now repeating on both existing private
indexes, without recompilation, reindexing, file filters or an answer model.
The benchmark's four broad-overview cases are reported separately: the preceding
v4 result delivered only **16.67%** of their expected sources. Their small sample
and the aggregate improvement cannot establish dependable overview quality.
No merge or working-installation update is accepted by these component checks.
