export interface Health {
  status: string;
  rag?: { status?: string; dense_available?: boolean; qdrant?: { ok?: boolean; points?: number; evidence_points?: number } };
}
export interface StatusModels {
  roles: { answer?: { model_id: string; status?: string } | null; embeddings?: { model_id: string; status?: string } | null };
}
export interface RuntimeStatus {
  chat_admission?: { allowed: boolean; reason?: string; memory?: { ram_free_gb?: number | null } };
}
export interface StatusJob {
id: string; type?: string; source?: string; status: string; phase?: string; total?: number;
  processed?: number; indexed?: number; index_errors?: number; index_skipped?: number;
  errors?: number; current_stage?: string; current_file?: string; dataset_name?: string;
  message?: string; reason_code?: string;
}
type Tone = 'ready' | 'attention' | 'unknown';
type StatusRow = { label: string; detail: string; tone: Tone; action?: 'models' | 'data' };

export function chatStatus(models: StatusModels | null, runtime: RuntimeStatus | null): StatusRow {
  if (!models) return { label: 'Не проверено', detail: 'Не удалось получить настройки модели.', tone: 'unknown' };
  const model = models.roles.answer;
  if (!model) return { label: 'Нужна модель', detail: 'Назначьте модель для ответов.', tone: 'attention', action: 'models' };
  if (model.status === 'blocked') return { label: 'Проверьте модель', detail: model.model_id, tone: 'attention', action: 'models' };
  if (runtime?.chat_admission?.allowed === false) return { label: 'Временно недоступен',
    detail: runtime.chat_admission.reason || 'Лес пока не может начать ответ.', tone: 'attention' };
  if (!runtime?.chat_admission) return { label: 'Модель назначена', detail: model.model_id, tone: 'unknown' };
  return { label: 'Настроен', detail: model.model_id, tone: 'ready' };
}

export function searchStatus(health: Health | null, models: StatusModels | null): StatusRow {
  if (!health || health.status === 'starting') return { label: 'Не проверено', detail: 'Нет свежих данных о поиске.', tone: 'unknown' };
  const rag = health.rag;
  if (health.status === 'error' || rag?.qdrant?.ok === false) return { label: 'Недоступен',
    detail: 'Не удалось проверить поисковое хранилище.', tone: 'attention', action: 'data' };
  const model = models?.roles.embeddings;
  if (models && (!model || model.status === 'blocked')) return { label: 'Нужна настройка',
    detail: 'Проверьте назначение модели для поиска по документам.', tone: 'attention', action: 'models' };
  if (rag?.status === 'empty') return { label: 'Пока пусто', detail: 'Добавьте папку с документами.', tone: 'attention', action: 'data' };
  if (rag?.status === 'not_indexed' || rag?.qdrant?.points === 0 || rag?.qdrant?.evidence_points === 0) return { label: 'Нет фрагментов',
    detail: 'Посмотрите ход обработки документов.', tone: 'attention', action: 'data' };
  if (rag?.status === 'ready' && rag.dense_available && (rag.qdrant?.points ?? 0) > 0) return { label: 'Индекс готов',
    detail: model?.model_id || 'Документы доступны для поиска.', tone: 'ready' };
  if (health.status === 'degraded' || rag?.status === 'degraded') return { label: 'Есть ограничения',
    detail: 'Проверьте состояние документов и индекса.', tone: 'attention', action: 'data' };
  return { label: 'Не проверено', detail: 'Готовность индекса пока не подтверждена.', tone: 'unknown' };
}

const stateOf = (job: StatusJob) => (job.status || '').toLowerCase();
export const activeJob = (job: StatusJob) => ['queued', 'running', 'parsing', 'waiting'].includes(stateOf(job));
export const failedJob = (job: StatusJob) => ['error', 'failed'].includes(stateOf(job))
  || (job.errors ?? 0) > 0 || (job.index_errors ?? 0) > 0;
export function jobProgress(job: StatusJob) {
  const total = Math.max(0, job.total ?? 0);
  const completed = job.phase === 'index'
    ? (job.indexed ?? 0) + (job.index_errors ?? 0) + (job.index_skipped ?? 0)
    : (job.processed ?? 0);
  return { completed: Math.max(0, Math.min(completed, total)), total };
}

export function jobStage(job: StatusJob): string {
  if (stateOf(job) === 'waiting') return job.message || 'Ожидает условий для продолжения';
  if (!activeJob(job)) {
    if (!failedJob(job)) return 'Обработка завершена';
    return job.source === 'native_external' && job.message?.trim()
      ? job.message : 'Обработка завершилась с ошибками';
  }
  if (stateOf(job) === 'queued') return 'В очереди';
  const stages: Record<string, string> = {
    CONVERT: 'Читает документ', EMBED: 'Готовит фрагменты для поиска', UPSERT: 'Обновляет индекс',
  };
  return stages[job.current_stage || ''] || (job.phase === 'index' ? 'Готовит документы к поиску' : 'Обрабатывает файлы');
}
