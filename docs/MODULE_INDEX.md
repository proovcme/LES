# Модули LES RAG

Порядок RRF/реранкера, контекст, worker и ошибки: [контракт RAG](modules/rag-ranking.md).
`backend/bm25_store.py` — postings и точная оценка при запросе;
`backend/sparse_index.py` — миграция и журнал областей изменений;
`backend/bm25_hybrid.py` — объединение с dense, `sparse_legacy.py` — TF-откат.
Рабочий BM25, миграция и обновление без LLM: [контракт](modules/bm25-index.md).
Версия словаря и короткие обозначения: `backend/inference/lexical_tokens.py`.
Уведомление об отказе с явным переходом: `sovushka/components/chat_failure_notice.py`.
Нагрузочный стенд без моделей: `tools/benchmark_sparse_index.py`.
Отдельный кандидат BM25 и чтения разделов: [контракт испытаний](modules/retrieval-candidate.md).
Чтение родителей в основном чате и бюджет доказательств: [контракт](modules/chat-section-context.md).

- backend: чтение документов, OCR, индекс Qdrant и политика ресурсов.
- proxy: API, чат, доказательства, память и подключения.
- sovushka: интерфейс NiceGUI.
- desktop/light: оболочка Windows.
- tools/light_launcher.py: собственные процессы приложения.
- installers/windows/light: установка и откат.

Пакетирование embeddings: `backend/embedding_client.py` — явная роль, ограниченный
размер ответа, сохранение порядка и проверка контракта между пакетами.
Журнал замены индекса: `backend/index_replacement.py` — решение commit, OS lease,
восстановление Qdrant/FTS и ревизия чтения; интеграция в collection/ingestion/retrieval.
PDF из цитаты: `proxy/routers/documents.py` и `proxy/services/pdf_viewer_service.py` —
разрешение исходника по происхождению, собственный просмотр страницы без PDF-плагина.

Восстановление диалога: `proxy/services/chat_durability_service.py`.
Фоновая сводка: `proxy/services/background_summary_service.py`.
Навыки и вычисления: `installed_skill_service.py`, `skill_compute_service.py`
в `proxy/services`, worker `tools/skill_compute_worker.py`.
Компактный каталог выбранных расширений: `proxy/services/extension_tool_service.py`.
Наблюдение за службами: `backend/light_health_monitor.py`.

Точные таблицы: `proxy/services/tabular_document_service.py` (координаты, формулы,
снимок источника). Задания по строкам: `document_task_store.py` (атомарные пакеты,
область, возобновление), `table_document_tool.py` (инструмент текущего диалога).
Контракт: [Работа с таблицами](modules/document-tasks.md).

[Карта кода](CODE_MAP.md) · [Архитектура](LIGHT_ARCHITECTURE.md)
