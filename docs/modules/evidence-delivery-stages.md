# Evidence delivery: isolated stages (885–886)

Working LES remains 880. All stages run only against the existing copied corpus,
without Qwen, manual source selection, model changes or reindexing.

## 1. Compact packet

`LES_EXPERIMENT_EVIDENCE_PACKET=compact-v1` is an explicit copy-only control for
`search_sources(view=context)`. Retrieval, original reading, lexical/MMR selection,
order and quotas are unchanged. It is incompatible with the RRF-order experiment.

The versioned `les.evidence-packet.v1` representation retains each full source once
in `sources`; a passage has a one-based `source.ref`. Text, canonical tables,
formulas, edition hashes, aliases, warnings and pinned links are preserved.
Encoding/decoding must round-trip exactly. Invalid/conflicting references are
not evidence. Main-chat admission and its model projection resolve the same table.
Ordinary clients/launches keep the existing representation.

Final admission measures the compact representation using the existing limits,
not the expanded compatibility view. Existing prefix admission is retained, so
this stage does not combine a new ranking policy with a wire-format change.
The MCP protocol's text/structured forms are not changed; byte duplication there
does not by itself prove duplicate model-token usage by a client.

Required comparison: baseline versus packet on the same questions/index, equal
pre-admission fingerprints; source/text/page/row/overview delivery, real MCP
size/latency, link and revision checks. No result is claimed before completion.

## 2. Saved canonical reading units

`LES_EXPERIMENT_SAVED_READING=canonical-v1` prepares a separate saved-unit reader.
It scans the published Word units, checks all text/structure digests and owner
coverage, and verifies the original SHA before/after plus the index revision.
Missing legacy representation explicitly falls back to the original reader;
corrupt units and changed generations are errors. Ordinary launches are unchanged.

The reading-view digest binds the ordered saved units and original identity.
It is not the old parser-view digest. Original ordinals are retained separately
from saved-stream offsets, so citation jumps and continuation use the right view.
The same `read_source` exposes this view through chat and MCP. Its scope is
published reading units, not a claim to read every visual or the complete original.
Explicit query/clause reads keep the original reader. No parser/index rebuild.

## 3. Overview coverage

After stage 2, test document-level grouping with balanced lexical/dense discovery,
then evidence reading inside each document. Grouping only an already truncated
pool cannot recover displaced documents. A bounded result never claims complete
corpus coverage. Precise page/table questions remain a regression gate.

## Completed stage 1 comparison

API source `dabc2da`, 131 API tests and 114 native passes / one optional skip.
All 200 real stdio MCP calls completed, two formats on the same 100 questions,
counterbalanced by query hash in four serial half-periods. BGE, disabled reranker,
questions, index generation, originals, selection and quotas were unchanged.
The evaluator decodes references after retrieval; raw MCP frames are retained.

| Measure | Existing packet | Compact packet |
|---|---:|---:|
| Needed-source delivery, 90 positive cases | 64.63% | 69.07% |
| Reference-text coverage, 69 cases | 53.29% | 56.52% |
| Expected-page hit, 12 cases | 91.67% | 91.67% |
| Expected-page coverage, 12 cases | 87.50% | 87.50% |
| Excel-row hit, 2 cases | 100% | 100% |
| Overview-source delivery, 4 cases | 29.17% | 29.17% |
| Observed MCP p50, 90 positive cases | 8.43 s | 9.20 s |
| Mean body characters, all 100 cases | 7,432.65 | 10,009.37 |
| Mean delivered passages, all 100 cases | 4.54 | 6.72 |

Four source cases and three reference-text cases improve; no scored source/text/
page/row cases regress. Paired 95% bootstrap intervals (20,000 fixed-seed draws)
for source/text deltas are [1.11, 8.89] / [0.00, 7.58] percentage points. This
repeatedly used development set is not independent answer-quality validation.
The format does not solve overview coverage and has no observed speed gain.

All 100 pairs have identical selected-unit fingerprints before wire admission.
Retrieval/read-plan fingerprints also match. Materialized reading pools differ
in two cases (one positive, one predeclared diagnostic) because of opposite
30-second Word-worker timeouts, but these differences did not change the selected
units. Both formats expose one `WORKER_TIMEOUT`; no new case is excluded.

All 200 raw text/structured MCP payload pairs agree and every source handle binds
the aligned full source. Both decoded cohorts respect the body quota and exact
same-source deduplication checks. Independent original/PDF checks by half-period
were 113/103, 149/170, 141/148, 124/130, with zero byte/page/digest/cursor errors;
these are per-period counts, not distinct originals across the whole experiment.
No selected derived-page units were newly covered. Copied catalog rows and every
original SHA remain unchanged. Resource floors were maintained and copy APIs
stopped before the next build. Working LES remains 880.

Stage 2 source `f284286` passed 132 API tests and built. Its first native run
passed 114, skipped one optional case and failed the new saved-reader case:
the fixture published a valid legacy unit, which the new loader incorrectly
treated as corrupt because both layouts use the v1 reading container. Inspection
confirmed its text and structure digests were valid and no canonical tree existed.
The correction distinguishes the nested canonical schema after digest validation;
the legacy fallback and a fixture with the real native Docling parser are now
separate cases. That repeat and UI/MCP checks are pending. Its original-reader
arm holds compact transport fixed. No installation/release; stage 3 pending.
