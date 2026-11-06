export interface Scope {
  scope_type: 'none' | 'all' | 'dataset' | 'datasets' | 'project' | 'projects' | 'mixed';
  project_ids: number[]; dataset_ids: string[]; selected_sources_only: boolean;
}
export interface Session {
  session_id: string; title: string; project_id: number | null;
  scope: Scope; role: string; registered: boolean; updated_at: string;
}
export interface Attachment { attachment_id: string; name: string }
export interface Draft { revision: number; text: string; attachment: Attachment | null }
export interface Source {
  index: number; doc_id?: string; dataset_id?: string; doc_name?: string;
  label?: string; header?: string; snippet?: string; quote?: string;
  cited_in_answer?: boolean;
  source_page?: string | number; page?: string | number; page_number?: string | number;
  locator?: { sheet?: string; row?: number; cell?: string; anchor?: string; source_sha256?: string; section_heading?: string };
}
export interface Message {
  role: 'user' | 'ai'; text: string; srcs?: string[]; crag?: string;
  meta?: AnswerMeta;
}
export interface AnswerMeta {
  execution?: ExecutionSummary | null;
  source_map?: Source[]; partial?: boolean; completion_status?: string;
  retrieval_trace?: { status?: string; tool_loop?: { status?: string }; execution?: ExecutionSummary; citation_check?: CitationCheck };
  blocker?: { action?: string }; total_status?: string;
}
export interface ChatPayload {
  execution?: ExecutionSummary | null;
  answer?: string; response?: string; sources?: string[]; source_map?: Source[];
  retrieval_trace?: { source_map?: Source[]; status?: string; tool_loop?: { status?: string }; execution?: ExecutionSummary; citation_check?: CitationCheck };
  partial?: boolean; completion_status?: string;
  blocker?: { action?: string }; total_status?: string; crag_status?: string;
}
export interface ExecutionSummary {
  schema: 'les.execution-summary.v1'; status: 'incomplete' | 'recorded' | 'no_changes'; notice: string; accepted: false;
}
export interface CitationCheck { status?: string; linked?: number[] }
export interface StreamEvent { event: string; data: unknown }
export interface Dataset { id: string; name: string; documents?: number; document_count?: number }
export interface ModelRole { model_id: string; connection_id: string; status?: string; code?: string }
export const emptyScope = (): Scope => ({ scope_type: 'none', project_ids: [], dataset_ids: [], selected_sources_only: false });
export const sourcesFor = (payload: ChatPayload): Source[] => payload.source_map ?? payload.retrieval_trace?.source_map ?? [];
export const sourcePage = (source: Source): number => {
  const value = Number(source.source_page ?? source.page ?? source.page_number ?? 1);
  return Number.isInteger(value) && value > 0 ? value : 1;
};
export function sourcePlace(source: Source): string {
  const { sheet, row, cell, anchor } = source.locator ?? {};
  if (sheet && row) return ` · ${sheet} · ${cell || `строка ${row}`}`;
  const paragraph = anchor?.match(/^para(\d+)$/);
  if (paragraph) return ` · абзац ${Number(paragraph[1]) + 1}`;
  const wordRow = anchor?.match(/^t(\d+)r(\d+)$/);
  if (wordRow) return ` · таблица ${Number(wordRow[1]) + 1}, строка ${Number(wordRow[2]) + 1}`;
  return source.source_page || source.page || source.page_number ? ` · стр. ${sourcePage(source)}` : '';
}
