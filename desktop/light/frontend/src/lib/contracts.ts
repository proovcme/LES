export interface Scope {
  scope_type: 'none' | 'all' | 'dataset' | 'datasets' | 'project' | 'projects' | 'mixed';
  project_ids: number[]; dataset_ids: string[]; selected_sources_only: boolean;
}
export interface Session {
  session_id: string; title: string; project_id: number | null;
  scope: Scope; role: string; registered: boolean; updated_at: string;
}
export interface Attachment { attachment_id: string; name: string }
export interface IndexedAttachment { intake_job_id: string; dataset_id: string; name: string }
export interface Draft { revision: number; text: string; attachment: Attachment | null }
export interface Source {
  model_locator?: import('./engineering').ModelLocator;
  index: number; doc_id?: string; dataset_id?: string; doc_name?: string;
  label?: string; header?: string; snippet?: string; quote?: string;
  cited_in_answer?: boolean;
  ordinal?: number; view_sha256?: string; evidence_kind?: string; section_heading?: string;
  source_sha256?: string; read_offset?: number; offset?: number;
  source_page?: string | number; page?: string | number; page_number?: string | number;
  sheet_name?: string; row?: number; equivalent_sources?: SourceOrigin[];
  locator?: { sheet?: string; row?: number; cell?: string; anchor?: string; source_sha256?: string; section_heading?: string };
}
export type SourceOrigin = Omit<Source, 'index'>;
export interface Message {
  role: 'user' | 'ai'; text: string; srcs?: string[]; crag?: string;
  history_id?: number;
  meta?: AnswerMeta;
}
export interface ContextExclusion { history_id: number; role: 'user' | 'assistant' }
export interface ContextReportSource extends Source { model_excerpt?: string; excerpt_limited?: boolean }
export interface ContextReportCall {
  purpose: 'answer' | 'tool_decision'; delivery: 'confirmed' | 'prepared' | 'legacy'; produced_answer: boolean;
  used: number; budget: number; free: number; generation_reserve: number | null; safety_reserve: number | null;
  provider_prompt_tokens: number | null;
  sections: { kind: string; tokens: number }[];
  omissions: { kind: string; count: number; reason: 'budget_exhausted' }[];
  sources: ContextReportSource[]; omitted_sources: { index: number; doc_name: string }[];
}
export const contextPath = (sid: string) => `/api/workspace/memory/context/${encodeURIComponent(sid)}`;
interface RetrievalTrace {
  source_map?: Source[]; status?: string; read_failures?: SourceReadFailure[];
  omitted_reads?: { index: number; label: string }[];
  tool_loop?: { status?: string }; execution?: ExecutionSummary; citation_check?: CitationCheck;
}
export interface AnswerMeta {
  error?: { code?: string; detail: string };
  execution?: ExecutionSummary | null;
  source_map?: Source[]; partial?: boolean; completion_status?: string;
  retrieval_trace?: RetrievalTrace;
  blocker?: { action?: string }; total_status?: string;
}
export interface ChatPayload {
  error?: { code?: string; detail: string };
  history_id?: number;
  execution?: ExecutionSummary | null;
  answer?: string; response?: string; sources?: string[]; source_map?: Source[];
  retrieval_trace?: RetrievalTrace;
  partial?: boolean; completion_status?: string;
  blocker?: { action?: string }; total_status?: string; crag_status?: string;
}
export interface ExecutionSummary {
  schema: 'les.execution-summary.v1'; status: 'incomplete' | 'recorded' | 'no_changes'; notice: string; accepted: false;
}
export interface CitationCheck { status?: string; linked?: number[] }
export interface SourceReadFailure { source: { doc_name?: string }; code: string; message: string }
export interface StreamEvent { event: string; data: unknown }
export interface Dataset { id: string; name: string; documents?: number; document_count?: number }
export interface ModelRole { model_id: string; connection_id: string; status?: string; code?: string; auto_refresh?: boolean }
export const emptyScope = (): Scope => ({ scope_type: 'none', project_ids: [], dataset_ids: [], selected_sources_only: false });
export const sourcesFor = (payload: ChatPayload): Source[] => payload.source_map ?? payload.retrieval_trace?.source_map ?? [];
export const sourcePage = (source: Source): number => {
  const value = Number(source.source_page ?? source.page ?? source.page_number ?? 1);
  return Number.isInteger(value) && value > 0 ? value : 1;
};
export const sourceDigest = (source: Source): string | undefined => source.locator?.source_sha256 ?? source.source_sha256;
export function sourceOrigin(current: Source, selected: SourceOrigin): Source {
  const { index, equivalent_sources, ...original } = current;
  const key = (s: SourceOrigin) => JSON.stringify([s.doc_id, s.dataset_id, s.source_sha256,
    s.locator, s.page, s.ordinal, s.read_offset, s.offset, s.view_sha256]);
  const seen = new Set([key(selected)]);
  const alternatives = [original, ...(equivalent_sources ?? [])].filter(s => {
    const id = key(s); if (seen.has(id)) return false; seen.add(id); return true;
  });
  return { ...selected, index, quote: current.quote, snippet: current.snippet,
    equivalent_sources: alternatives };
}
export function sourceReadStart(source: Source): number {
  const offset = source.evidence_kind === 'physical_word_section' ? (source.read_offset ?? source.ordinal)
    : source.evidence_kind === 'physical_pdf_unit' ? source.read_offset
    : source.evidence_kind === 'physical_pdf_page' ? source.offset : undefined;
  return typeof offset === 'number' && Number.isSafeInteger(offset) && offset >= 0
    && /^[a-f0-9]{64}$/i.test(source.view_sha256 ?? '') ? offset : 0;
}
export function sourcePlace(source: Source): string {
  if (source.model_locator) return ` · ID ${source.model_locator.native_id}`;
  const { cell, anchor } = source.locator ?? {};
  const workbook = sourceWorkbookRow(source);
  if (workbook) return ` · ${workbook.sheet} · ${cell || `строка ${workbook.row}`}`;
  const paragraph = anchor?.match(/^para(\d+)$/);
  if (paragraph) return ` · абзац ${Number(paragraph[1]) + 1}`;
  const wordRow = anchor?.match(/^t(\d+)r(\d+)$/);
  if (wordRow) return ` · таблица ${Number(wordRow[1]) + 1}, строка ${Number(wordRow[2]) + 1}`;
  return source.source_page || source.page || source.page_number ? ` · стр. ${sourcePage(source)}` : '';
}

export function sourceWorkbookRow(source: Source): { sheet: string; row: number } | undefined {
  const sheet = source.locator?.sheet ?? source.sheet_name;
  const row = source.locator?.row ?? source.row;
  if (!source.model_locator && sheet && typeof row === 'number' && Number.isSafeInteger(row) && row > 0) {
    return { sheet, row };
  }
}
