# RAG ablation and architecture-risk protocol

Status: protocol and CI changes, not an acceptance result. Working LES stays
on build 880. Trials use the existing private copy, BGE and disabled reranking.
No user document, index generation, model assignment or installed file changes.

## Evidence already available

[RRF admission control](../modules/rrf-admission-control.md) already compared
the old selector/admission policy against RRF-order exact admission on the same
100 development questions. Delivery improved on average, but page and overview
regressions rejected replacement. This combined experiment does **not** isolate
MMR from serialization, ordering and admission changes.

[Evidence-delivery stages](../modules/evidence-delivery-stages.md) separates
compact transport, saved reading and overview discovery. These results must not
be pooled into one claim about answer accuracy. In particular, the first overview
strategy regressed and was rejected; corrected document fusion needs a repeat.

Frozen100 is a development/regression set. Its repeated use disqualifies it as
independent acceptance evidence. The eight prospective overview probes have no
complete gold answer and must not receive invented recall percentages.

## Controlled contrasts, before implementation

Keep the exact source commit, corpus/index identities, model/tokenizer versions,
query encoding, budgets, wire representation, scopes and original readers frozen
in a private manifest. Gold files are inputs only to the evaluator after retrieval.
No filename/page/doc-ID filter may be derived from the expected answer.

| Variant | Post-RRF behavior | Contrast |
|---|---|---|
| V0 anchor baseline | Read the matched original element only; no parent/neighbor expansion; RRF order, exact admission | Minimal evidence baseline |
| V1 frozen current | Full current reading, selector, existing envelope | Reproducible control, not a moving branch |
| V2 exact admission | Same reading, selected units **and their order** as V1; one final serialized admission, skip whole non-fitting snapshot units | Budget/admission only |
| V3 no selector | Same full read pool as V2, RRF order, exact admission | Combined removal of lexical selection and its ordering |
| V4 bounded reader | Same V3 admission; bounded surrounding structural elements with explicit continuation | Bounded versus full reading |

The proposed V2 also reordered selected units: that would confound a budget-only
claim. If needed, an additional ordering-only contrast must be registered before
running it. If V0 and V4 produce identical read pools, collapse them; do not call
the same implementation two independent variants. Preserve table headers,
continuations, source/page/edition pins and failure reasons in every arm.

The existing `LES_EXPERIMENT_CONTEXT_ADMISSION` flag provides `baseline-v1` and
`rrf-wire-v1`; these are the completed combined contrast, **not** five implemented
variants. No `RAG_ABLATION=v0..v4` switch or completed five-arm run is claimed.
Experimental switches stay off in normal launches and reject unknown values.
They are temporary research arms. Before production acceptance, preserve source
SHAs/manifests/results and remove losing paths from the shipped pipeline rather
than retaining every strategy as another permanent mode or hidden fallback.

Admission properties are determinism for frozen inputs, exact cost within every
declared limit, whole units and valid source lineage. Do not demand monotonic
source recall merely from increasing a greedy packing budget: admitting an
earlier large unit can replace several later small ones. Test the actual
invariants instead of an invalid monotonic-composition assertion.

## Independent acceptance cohort

- At least 150 new independently authored questions, including at least 30
  overview questions and at least 15% genuinely unanswerable cases. Include
  exact facts, clauses, numbers/units, spanning tables, editions and contradictions.
- Split by document family, not just question: byte-identical copies, related
  editions and duplicated sections stay in the same split. Dev and acceptance
  document families cannot overlap. Keep the same index if required by the
  contrast; document this as transductive corpus evaluation, not unseen ingestion.
- An independent custodian holds questions and annotations during development.
  Freeze the manifest hash, code, comparison schedule and scoring rules first.
  A developer-generated set is useful prospective testing, not independent gold.
- Specify relevant documents, exact supporting text, physical pages, table
  row/column/unit and edition relations. Unanswerable cases need checked scope.
  Annotators must not derive gold from the candidate's retrieved output.
- If no independently prepared cohort is supplied, report development results
  only. A failing acceptance case used for a fix becomes development data;
  use a fresh sealed set for the next acceptance decision.

## Metrics and decision rules

Distinguish mean per-question **fraction of required sources delivered** from
binary **any required source delivered** and **all required sources delivered**.
Report sample sizes/denominators and zero-source/error cases; do not rename one
as the other. SHA-equivalent sources measure same-byte delivery, not correct
edition authority or distinct file-path coverage. Score exact page/row/edition
separately. Quote five-gram coverage is a proxy, not answer correctness.

Record retrieval -> read plan -> readable -> selected -> wire, using scoped
identities and unit fingerprints, plus timings, explicit failures, p50/p95 and
false `NO_DATA` on positive cases. Classify loss only where a stage observation
exists. A source absent from a top-64 pool has unknown rank: `rank>64` cannot be
inferred without a separately registered deeper search diagnostic. Dedup aliases
need source lineage; an expansion is not a new dense hit. Changed-source/index
errors must remain errors, never be counted as successful skipped evidence.

Predeclare four primary paired contrasts against V1. Use 20,000 deterministic
paired bootstrap resamples and simultaneous 98.75% intervals (Bonferroni family
alpha 0.05), alongside descriptive 95% intervals. Related questions share a
document-family resampling block. Exact McNemar applies only to the predeclared
binary outcomes; do not apply it to fractional source recall or quote coverage.

**A confidence interval crossing zero does not establish equivalence.** An
inconclusive result leaves the decision open. Non-inferiority requires a
predeclared, justified margin and adequate power; none is invented here. Removing
a layer requires evidence of preserved critical page/number/edition/citation
behavior, no unresolved integrity regression, and a justified quality/speed result.
Do not tune after opening acceptance results or select favorable questions.

The MCP-only stages intentionally call no answer model. Before release, add a
separate fixed-model answer evaluation: a preregistered random sample of at least
50 held-out cases, blinded manual review, supportedness and refusal errors, plus
number/unit/row checks. It cannot be claimed completed from source delivery alone.

## CI implemented in this change

`verify.yml` runs Rust formatting for all six manifests before dependency builds,
then static contract and publication checks in a separate job. A Linux
frontend job runs types and behavior independently. Both gate the existing full
Windows native build/tests and desktop shell. The full behavior suite remains;
collection is never presented as execution. `make verify-fast` checks version,
runtime map and syntax without a native asset. API-contract capture imports the
job service and requires the core: a missing-core dry run confirmed this, so
API capture and test collection remain in full `make verify` after native build.

Fast stale PR jobs may be canceled; main checks and running native/desktop gates
are not canceled by each new push. One running heavy gate per ref is retained;
GitHub may replace its pending successor with the newest push. A completed older
SHA is not acceptance of a newer SHA. Keep the existing shared Rust cache and
single-job native compilation until measured runner resources justify changing it.

No unverified Linux COM/OLE test matrix, nonexistent pytest marks, new xdist,
new cargo-audit/deny installation or placeholder integration job is copied from
the draft. Fast jobs have timeout bounds, **no measured 3–5 minute promise**.
Clippy/unsafe auditing remains a separate measured step; making all existing
warnings fatal before review would hide useful feedback behind a large cleanup.

First [workflow run](https://github.com/proovcme/LES/actions/runs/43742511377),
source `ac4992b`: static/format/publication job passed in 44 seconds; frontend
types/behavior passed in 28 seconds; desktop build passed. These are observed
job wall times for this run, not a cold-cache SLA or a completed native-suite
result. The full native job is separate.

## Ordered architecture risks and exit criteria

| Priority | Boundary | Required evidence before removal/replacement |
|---|---|---|
| P1 | Evidence packet source references; payload/unit/read failure | Typed internal identities and explicit wire conversion; reject invalid refs/missing pins; preserve canonical cells, aliases and old clients |
| P1 | Published reading representation versus index anchors | Same parser/schema generation and stable unit identities for indexing and reading; migrate only a separate copy, never relabel legacy Markdown as canonical or weaken hashes |
| P1 | MSG/DOC/PDF unsafe input boundaries | Read actual ABI contracts, ownership, lengths and error paths; meaningful `SAFETY` per block; damaged/truncated inputs and bounded subprocess failures; fuzz separately |
| P1 | Chat runner and indexing coordinator | Behavior-preserving phase extraction with shared turn/job state, cancellation/recovery and rollback parity; no parallel semantic rewrite |
| P1 | Retrieval/select/admission | Implement one registered contrast at a time, real MCP stage receipts, exact citations and no regressions hidden by averages |
| P2 | Large functions and untyped JSON | Inventory production versus tests first; extract typed source construction and read failures before unrelated code |
| P2 | Clippy/unwrap policy | Audit reachable production errors; warnings baseline first, reviewed deny policy for new code; raw text counts are not reachable panic counts |

Function lengths and raw `unsafe`/`unwrap` counts in the external review are
triage, not a verified vulnerability or test-coverage inventory. A line-count
ceiling alone can move complexity without reducing it. Do not add a blanket
`SAFETY` comment or suppress a warning instead of establishing the invariant.
This protocol does not claim the typed-boundary refactor or fuzz audit completed.

The representation risk is already concrete: stage 2 used saved reading five
times, fell back 702 times and delivered no saved-unit source. Therefore the
existing-copy run cannot validate broad saved-reader coverage. A representation
migration is a separate controlled experiment, not an unnoticed index change
inside the fixed-index ablation.
