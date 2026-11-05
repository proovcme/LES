# Модули LES RAG

Установленный 785: `core/src/catalog/job_store.rs` владеет нативным подсчётом
PENDING и исключительным захватом очереди; порции выполняет существующий адаптер.
[Контракт](modules/auto-parse-resume.md), [проверка](acceptance/parse-queue-785.md).

Установленный локальный 784: native jobs, освобождение простаивающих процессов/heap и управление
памятью фактически вызванной Ollama под прежним guard. Переход «индексация → два ответа» проверен без перезапуска.
[Контракт](modules/runtime-memory.md), [проверка](acceptance/runtime-memory-784.md).

Установленный локальный 783: `core/src/document/legacy_doc.rs` читает DOC на Windows без Word,
`core/src/citations.rs` владеет ссылками/счётчиками/manifest. Устранены тяжёлые
PDF-пути и запуск embedded format worker. Установка и повторная обработка корпуса описаны в приёмке.
[Контракт](modules/document-extraction.md), [проверки](acceptance/document-extraction-783.md).

Установлен локальный патч 782: `auto_parse_resume_service.py` — preflight до job и завершение
ошибок/отмены автоочереди без ложного PARSING. [Контракт](modules/auto-parse-resume.md),
[приёмка и открытые дефекты](acceptance/auto-parse-resume-782.md).

Включён этап 781: `core/src/harness.rs` — бюджет, монотонный срок, повторы и остановка
цикла инструментов основного чата. `harness_loop_service.py` — тонкий адаптер.
`core/src/receipts.rs` — подтверждения действий и смешанная сводка без потери
успешных изменений или скрытия ошибок отдельных попыток.
[Граница](modules/native-harness-loop.md), [приёмка](acceptance/native-harness-loop-781.md).

Включён этап 780: `core/src/bm25.rs` — postings, оценки и RRF;
`core/src/context.rs` — бюджет и порядок модельного контекста. Python оставляет
адаптеры, старый SQL/packer — только test oracle. [Контракт](modules/native-rag-harness.md),
[приёмка](acceptance/native-rag-harness-780.md).

Ранее принятый этап 779: `core/src/agent_files/{tool,projection}.rs` владеют схемой,
привязкой к диалогу, scope, Unicode-порциями и проекцией памяти. `transaction.rs`
добавляет удаление с обнаруживаемой историей; `export` сохраняет UTF-8 байты.
[Полный цикл с реальной Qwen и границы](acceptance/agent-native-779.md).

Кандидат 778: `core/src/agent_files/` — USER.md/MEMORY.md и рабочие UTF-8 файлы
с ревизиями, историей и восстановлением. `api/src/agent_state.rs` — две прямые
API-пары; всего 22/267. Один привязанный инструмент использует существующий
harness; редактор Svelte ждёт сохранения при закрытии. [Контракт](modules/agent-workspace.md).
[OpenCode: решение следующего этапа](modules/code-agent.md), адаптер ещё не реализован.

Кандидат 777: `core/src/catalog/{projects,notes,conversation}.rs` владеют SQLite
проектов, заметок и состояния контекста. `api/src/catalog.rs` обслуживает их
14 дополнительных HTTP-пар, всего 20/265. Генерация сводки, POST заметки со
связями и отдельный Memory Core пока Python. [Контракт](modules/native-catalog.md).

Кандидат 776: `api/` — нативные шесть HTTP-пар диалогов/черновиков и потоковая
граница оставшегося API; `core/src/workspace/` — единственный владелец их SQLite.
Python-сервисы оставляют публичные фасады, прежний SQL только в тестовом oracle.
[API](modules/native-api.md), [хранилище](modules/native-workspace.md).

Кандидат 775: `runtime/` — Rust-владелец Qdrant/API/UI и перезапусков;
`desktop/light/src-tauri/src/launcher.rs` — явный выбор запуска без скрытого
fallback. [Контракт](modules/native-runtime.md), [приёмка](acceptance/native-runtime-775.md).

Кандидат 774: `core/src/worker_host.rs`, `process_guard.rs`, `source_scan.rs` —
обход файлов и владение изолированным обработчиком. `backend/format_process.py`
связывает ingestion с `tools/light_document_worker.py`. [Контракт](modules/format-worker.md)
и [границы полного удаления Python](modules/python-exit.md).

Импорт без чтения PDF/Office при регистрации: `backend/external_registration.py`.
Причины остановки: `backend/runtime_events.py`; внешний Windows-наблюдатель и
восстановление launcher: `desktop/light/src-tauri/src/supervisor.rs`, `lifecycle.rs`.
[Контракт](modules/runtime-recovery.md), [приёмка 773](acceptance/runtime-recovery-773.md).

Безопасный старт коллекции: `backend/collection_layout.py` (lease/exact count/
сверка каталога). `execution_receipt_service.py` — состояние зафиксированных
действий, `desktop/light/src-tauri/src/close_guard.rs` и frontend/native-close —
протокол закрытия. [Приёмка 772](acceptance/safety-ui-772.md).

Кандидат интерфейса Svelte/TypeScript: `desktop/light/frontend`; статическая
раздача `proxy/light_frontend.py`, черновики `chat_draft_service.py` и workspace
API. Запуск Tauri с `--svelte`, прежний UI — откат. [Границы переноса](modules/svelte-desktop.md).

Rust-ядро документов: `core/src/lexical.rs`, `files.rs`, `jobs.rs`;
`backend/native_core.py` — загрузка и ограниченный C ABI, `lexical_preparation.py`
— подготовка postings, `file_fingerprint.py` — проверка байтов, `job_policy.py`
— переходы заданий. [Контракт и откат](modules/native-core.md).

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
Сравнение полного и минимального поиска, модельного пакета и локальных судей:
[стенд 764](modules/rag-pipeline-trial.md). `lexical_evidence_service.py` сверяет
FTS ID с Qdrant; `section_coverage_service.py` формирует модельную метаинформацию
о полноте реально переданных индексированных разделов.

- backend: чтение документов, OCR, индекс Qdrant и политика ресурсов.
- proxy: API, чат, доказательства, память и подключения.
- sovushka: интерфейс NiceGUI.
- desktop/light: оболочка Windows.
- tools/light_launcher.py: собственные процессы приложения.
- installers/windows/light: установка и откат.

Удаление набора с recovery и проверкой границы до изменений:
`proxy/services/dataset_deletion_service.py`, [контракт](modules/dataset-deletion.md).

Пакетирование embeddings: `backend/embedding_client.py` — явная роль, ограниченный
размер ответа, сохранение порядка и проверка контракта между пакетами.
Кандидат 762: короткий кэш векторов запросов, нативный протокол после live probe,
контекст и управление резидентностью; [измерения](acceptance/embedding-762.md).
Журнал замены индекса: `backend/index_replacement.py` — решение commit, OS lease,
восстановление Qdrant/FTS и ревизия чтения; интеграция в collection/ingestion/retrieval.
`document_catalog.document_registration` — снимок идентичности и пути источника;
ingestion повторно проверяет его под writer lease перед публикацией. Удаление или
перерегистрация во время конвертации отменяет устаревшую работу без записи в индекс.
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
Общая очередь выбранных документов/разделов: `document_review_tool.py`,
`review_task_store.py`, `review_source_reader.py`; [контракт](modules/document-review.md).
`document_task_database.py` — общий транспорт SQLite; `table_window_service.py`
— адресные окна и чтение длинных ячеек; `bound_document_tools.py` — обязательный
контекст привязанных инструментов и их место в shortlist.
`native_tool_message_service.py` — адаптер канонических assistant/tool к native
chat: объект arguments и имя tool_name без изменения сохранённого диалога.

[Карта кода](CODE_MAP.md) · [Архитектура](LIGHT_ARCHITECTURE.md)
