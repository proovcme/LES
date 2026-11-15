# RRF/context admission control (candidate 884)

Status: isolated experiment, not the default pipeline, installed patch or release.
Working LES remains 880. No embedding assignment, index or original is changed.

## What the external review established and what it overstated

- The post-read lexical/MMR selector changes the fused retrieval order. Its
  benefit must be measured against RRF order with the same read pool and limits.
- Serialized evidence costs differ from the selector's text estimate. Exact
  ready-record admission is required; repeated identities and typed cells count.
- The Frozen100 MCP `view=context` path passes `context_budget=None`: it has a
  12,000-character text quota, a separate wire ceiling and **no 1,024-character
  per-unit reserve**. `Window::admit` belongs to explicit prefix reading, not
  another consecutive selection pass in this measured path. The 19,500-character
  inner envelope applies to the navigation path; the measured context wrapper
  uses 22,500. The limits must not be conflated.
- Prefix `break` is necessary for explicit stream cursors. Only a nonpaginated
  context snapshot can skip a whole nonfitting unit and continue in ranked order.
- Exact duplicate text retains source aliases. Aliases may be explicitly omitted
  under wire pressure; deduplication alone does not establish a lost source.
- Word units with matching ordinal/text hashes inherit their actual anchor rank.
  Expanded units retain a document prior, not an invented dense hit. Original
  Word reparsing remains a separate measured cost.
- Read timeouts are explicit partial failures. Changed originals/views and mixed
  index generations are errors/retries, not successful empty evidence. These
  guards must not become unconditional skip-and-continue behavior.
- The claimed verbatim RRF acceptance invariant is not in this repository's
  `AGENTS.md`. The experiment is justified by the pipeline and measured losses.
- For the assigned BGE-M3, raw queries are supported: the
  [official model card](https://huggingface.co/BAAI/bge-m3) says query instructions
  are not required. This experiment does not alter query encoding.

## Two policies, one copy

An explicit developer-only process variable, `LES_EXPERIMENT_CONTEXT_ADMISSION`,
enables `baseline-v1` or `rrf-wire-v1` for `search_sources(view=context)` only.
Unset retains the existing path. Unknown values reject the experimental request.
There is no product switch, automatic fallback, topic rule or answer model.

Both policies share identity validation, original readers, canonical views,
duplicate normalization/aliases, retrieval scores and the complete read plan.
Both explicit trial arms use a stable pool order to remove container-iteration
and duplicate-primary tie noise; the unconfigured production path is unchanged.
The control orders exact units by existing fused rank; expanded units follow
exact hits using their source prior and stable original order. It performs no
lexical/MMR/length reranking. The ordering stage spends no output budget.

One final admission function measures the ready response, including both existing
source representations, canonical cells, receipts and warnings. It enforces the
unchanged text quota, 22,500-character envelope, item allowance and 32-unit cap.
A nonfitting context unit is skipped whole; later fitting units remain eligible.
Source streams keep their previous contiguous-prefix behavior. Navigation and
trace details use remaining room after evidence. The public source format is
preserved for existing UI/MCP consumers; source-map migration is not part of this
control. Per-request catalog lookup caching is common to both policies.
The control compacts optional aliases only on a candidate which actually fails
admission, so a rejected large candidate cannot erase another fitting unit's
provenance. This measures the combined ranking/admission policy, not the isolated
causal contribution of lexical scoring alone.

`paired-a-v1` / `paired-b-v1` counterbalance cache/order effects: query SHA alone
chooses each question's first policy, and the second period flips it. Each of
the same 100 questions receives both policies. No gold, filename or expected
page participates in assignment. Scores remain separated by actual policy;
latency also records period/order. Original reads that differ between periods
must be reported as confounding pairs, not silently excluded from primary scores.

## Required gates

- Core: no lexical override of RRF; no invented expansion rank; duplicate source
  provenance retained, changed numerical content kept separate.
- Envelope: oversized middle unit can be skipped only for a context snapshot;
  typed-cell/source cost and actual text quota are both respected.
- Native: query-only discovery, pinned originals, uninterrupted Word cursors,
  no answer-model calls and original replacement rejection.
- Frozen100 MCP on the existing private v4 copy, both whole datasets, unchanged
  12,000-character request budget, no file filter and no reindex.
- Stage receipts: retrieval → read plan → readable pool → selection → wire.
  Logs contain hashes/counts, not document text. Check retrieval/read-pool
  fingerprints across policies before attributing differences to admission.
- Minimum 4 GiB available RAM / 20 GiB disk, serial compiler/services. Stop own
  services afterward; do not duplicate corpus stores or change working LES.

No benefit or replacement decision is claimed before these gates complete.

## Validation boundary

Frozen100 is now a development/regression set. This ablation diagnoses delivery
losses; repeated gains on it do not establish independent quality or answer
correctness. A release needs a separately annotated question set which has not
guided these changes, with broader overview coverage, wrong/unsupported answers,
numerical row/column/unit association, editions and citations evaluated separately.
Freeze code/model/index identities before opening that set; a set used to fix
failures becomes a development set and cannot keep its held-out label. No closed
independent acceptance set or answer-quality result is claimed here.

Repository-wide counts of `Value`, `unwrap` or test functions are triage signals,
not measured panic reachability or coverage. Typed payload/unit boundaries and
generated invariant checks remain architectural work; this experiment does not
claim a repository-wide type or panic audit. Unbounded parallel parsing is not
enabled: original-reader memory reserves remain unchanged.

Component gates: **172 core / 128 API cases** passed. Native checks compose
**113 passes and one optional skip**: the first run passed 112 and exposed a new
test incorrectly forwarding `index_revision:null` from an index-independent Word
response. The corrected test omits the absent selector, keeps the view pin, and
passed all original-stream/number/replacement assertions. The original failure
is retained. `make verify` passed with **2,230 collected**, not executed; its
earlier attempt during the core rebuild failed at core-library discovery and
is retained separately. The stable repeat explicitly pins the built core asset.
Two transport-only MCP smoke calls exercised both policies with valid source
checks; they are not quality evidence. The counterbalanced 200-call run is in
progress. Working installation/default context policy are unchanged.
