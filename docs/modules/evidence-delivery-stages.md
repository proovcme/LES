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

Stage 1: 131 API tests and 114 native passes / one optional skip; transport smoke
passed. Its 200-call counterbalanced MCP comparison is in progress. Stage 2 is
prepared, not built/accepted; it must run after stage 1 completes. Stage 3 pending.
