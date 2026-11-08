import { expect, test } from 'vitest';
import { activeJob, chatStatus, jobProgress, jobStage, searchStatus } from './system-status';

test('readiness uses current role, admission and searchable evidence', () => {
  const models = { roles: { answer: { model_id: 'local-chat' }, embeddings: { model_id: 'local-vectors' } } };
  expect(chatStatus(models, null).label).toBe('Модель назначена');
  expect(chatStatus(models, { chat_admission: { allowed: false, reason: 'Идёт индексация' } }))
    .toMatchObject({ tone: 'attention', detail: 'Идёт индексация' });
  expect(chatStatus({ roles: {} }, null).action).toBe('models');
  expect(searchStatus({ status: 'ok', rag: { status: 'ready', dense_available: true, qdrant: { points: 4, evidence_points: 0 } } }, models).label)
    .toBe('Нет фрагментов');
  expect(searchStatus({ status: 'ok', rag: { status: 'ready', dense_available: true, qdrant: { points: 4, evidence_points: 4 } } }, models).tone)
    .toBe('ready');
});

test('an import waiting to index remains visible and counts indexed rather than discovered files', () => {
  const job = { id: 'import', status: 'waiting', phase: 'index', total: 10,
    processed: 10, indexed: 3, index_skipped: 1, index_errors: 1, message: 'Ожидаю освобождения памяти' };
  expect(activeJob(job)).toBe(true);
  expect(jobStage(job)).toBe(job.message);
  expect(jobProgress(job)).toEqual({ completed: 5, total: 10 });
  expect(jobStage({ ...job, status: 'failed', source: 'native_external', message: 'Папка недоступна.' }))
    .toBe('Папка недоступна.');
});
