# Canonical Word structure

Build 883 introduces `les.document-structure.v1`, a bounded projection of the
Docling item tree. It retains source item/parent IDs, headings, inline runs and
their formatting/hyperlinks, logical table cells, spans, roles, nested content
and available provenance. Group references preserve list and rich-cell ancestry.
The projection is versioned; it does not pretend to be Docling's full JSON export.

Heading ancestry uses Docling item IDs, including distinct headings with the
same title. Index hierarchy construction consumes that path directly. Literal
numbering or Markdown characters in a paragraph do not create another heading.
Search and semantic context admission use raw source values; display escaping
must not split identifiers such as `DN_100`. The common final sanitation still
applies to explicit units before token budgeting.

The isolated parser walks the borrowed tree. It does not construct Docling's
whole JSON value or infer tables/headings from styled Markdown. Native checked
OCR, embedded objects and mathematics are restored into authoritative text
fields before escaping. A missing or duplicated native-part marker fails the
conversion. Referenced furniture and floating objects retain separate owners.

The core renders presentation and builds reading units from this structure.
Indexing and original reading reuse the same unit IDs, byte ranges and table
rows. Final token budgeting consumes those units directly. DOCX does not run
through `markdown_sections` or the Markdown structure detector. Standalone
Markdown retains its format adapter. Older indexes remain readable through the
registered-original reader; no existing collection is silently reindexed.

The pinned binary `.doc` backend does not expose an item tree. Its existing
flat adapter remains an explicit compatibility exception, without labelling
its Markdown output as canonical structure. Removing that exception requires
an equivalent structured binary-Word adapter; dropping the format is not allowed.

Referenced furniture and floating objects remain owned by the native special
reader. Their existing table/paragraph units are detected once in that adapter
and retained, preserving table addresses and arithmetic. They do not pass
through the Docling Markdown round trip or a second final structure detector.

New collections use `metadata-v1` and `canonical-source-final-budget-v4`.
The earlier v2/v3 contracts remain explicit compatibility paths. Reading units
remain `les.document-reading.v1` and carry an additional typed `canonical`
field. The reading view is bound to its structure and budget; indexed typed
views also carry a checksum of the complete unit, including cells.

Chat and MCP share the same retrieval service. Before original reading there
is only a fused-rank I/O limit of 32 physical read scopes, without query-dependent
semantic selection or an answer-text budget. Bounded hydrated parent/document
continuations remain available and receive no invented retrieval rank. Original
document extent is retained so a clipped plan cannot claim full-document coverage.
Deduplication and semantic admission occur
once after reading, under the final context limit. Typed cells and paragraphs
provide model addresses and numeric receipts directly; Markdown is presentation.
Literal operators, pipes and brackets are escaped only for display.

Paragraph addresses remain selectable when a neighboring reading unit contains
the table. Address availability does not itself turn a prose task into arithmetic.
The existing single answer-window retry checks delivery of verified reads,
including partial documents. It does not depend on claiming whole-document
coverage and does not reopen tools or waive the final context budget.
Typed table arrays use a registered nested-item allowance of 8,192 while retaining
the independent 24,000-character tool-result ceiling. The wire reader retains
its tighter character limit and admits whole units; legacy flat-text envelopes
keep their earlier item bound. Chat uses the registered budgets directly rather
than overriding them privately.

Wire admission compacts redundant read-failure URLs and locators before it
can remove proof bodies. Causes, messages, source/revision identities and retry
cursors remain available; a failed neighbor must not spend a verified page's
entire transport allowance.

PDFium owns revision-checked original PDF text. Xberg table enrichment has a
separate proof boundary: a literal mismatch rejects that projection, preserving
raw original text with `table_projection_warning`. Other source, scope, identity
and parser-shape errors still fail. Index nodes carry the warning only on the
affected page. Chat and MCP expose the same warning; the source window displays
the shared message. Native selection admits such a page only as an exact excerpt,
without invented row/column addresses or arithmetic.

Memory guards bound the tree projection, recursion, grid dimensions and
replicated span text. Document-wide structure is excluded before cloning
per-point metadata. The Word LRU stores byte counts instead of repeatedly
serializing every retained document to count its size.

Acceptance status and measured limits belong in
[the build 883 acceptance record](../acceptance/canonical-rag-883.md).
