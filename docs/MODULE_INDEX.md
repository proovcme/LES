# Модули LES RAG

872: [документные parsers](modules/native-document-parsers.md) — `parsers/`,
`api/src/document_parser`, `core/src/document/docx/package/docling.rs`.
Docling владеет обычной структурой Word; LES — специальными вставками,
OCR и полнотой; Xberg — геометрией PDF-таблиц при прежней идентичности PDFium.
[Сквозная приёмка на копии](acceptance/native-document-parsers-872.md).

Стенд 871: [граница нативных парсеров](modules/native-parser-comparison.md),
[завершённое сравнение](acceptance/native-parser-comparison-871.md) и
[обезличенные измерения](acceptance/native-parser-comparison-871.json).
В том сравнении владельцы приложения не менялись. Для приёмки адаптера рассматривались
DoclingDocument/DOCX и Xberg/PDF-таблицы; результаты внедрения указаны выше.

870: core/src/coordinator — Rho runtime, host-политики, I/O и wire-адаптер.
core/src/agent_loop и старый счётчик harness удалены; API использует
проекцию истории SDK. [Договор](modules/native-rho-coordinator.md),
[приёмка и ограничения](acceptance/native-rho-coordinator-870.md).
Для следующих готовых компонентов — [сверка совместимости](modules/native-component-candidates.md).

Стенд 869: [сравнение SDK и карта удаления координатора](acceptance/native-harness-comparison-869.md).
Rho выбран для владения циклом; Evidence, reader, бюджет, редакции и
цитаты сохраняются. Это архитектурное решение по результатам отдельного
стенда, не подключение SDK к текущему API.

868: core/src/chat_context/prose.rs — точные абзацы смешанного доказательства;
tabular.rs — разметка вместе с ячейками. api/src/chat/table_facts/addresses.rs
различает адреса абзацев и строк. [Приёмка](acceptance/native-mixed-evidence-868.md).
[Готовые компоненты](modules/native-ready-stack.md) проходят отдельное включение.

867: `core/src/retrieval_fusion.rs` — общий RRF/DBSF и отдельный допуск
кандидатов; Server/Edge используют один модуль. `sources/retrieval.rs`
передаёт диагностику только начальной подборке, `runner.rs` отделяет
освобождение схем от ошибки табличного протокола.
[Математика, приёмка и открытые проблемы](acceptance/native-fusion-867.md).

866: `core/src/lexical.rs::versioned` — явно версионированные v2 и Snowball v3;
`current` и штатные пути записи/поиска сохраняют v2. Версии и контрольные суммы
библиотеки закреплены во всех четырёх Rust lock-файлах. Проверка v3 — на копии.
`retrieval::read` и `runner` — [допуск reader](acceptance/native-reader-admission-866.md).

865: [обзорные справки](acceptance/rag-synthesis-865.md) — непройденная
приёмка отбора томов, синтеза, допуска reader и ограниченного дочитывания.
Открытые изменения перечислены в бэклоге, не объявлены выполненными.

865: [источники и контекст](acceptance/native-source-selection-865.md) —
`sources/retrieval.rs`, `retrieval/packing.rs`, `Evidence::optional_blocks`,
`table_facts.rs::finish_checked` и схемы/квитанции `chat/tools.rs`.
`core/src/agent_loop.rs`: подготовка финального окна отделена от одной
попытки исправления завершения; инструменты и бюджет действий не возобновляются.
Отдельной стадии выбора файла моделью нет; индекс и модели прежние.

864: [архитектурная сверка](acceptance/rag-architecture-audit-864.md) —
`chat/budget.rs`, `model.rs`, `tools.rs`, `tools/access.rs`: общий абсолютный срок;
`sources/retrieval.rs`: назначение поиска и политика начальной подборки;
`tools/evaluate_light_chat.py`: раздельные метрики доставки и цитирования.

863: [бюджет документа и продолжение поиска](acceptance/native-context-budget-863.md) —
`core/src/chat_context/assembly.rs` и `core/src/context.rs`: полный документ
или целые блоки с неполным покрытием; `retrieval.rs` применяет смещение
ранжированных совпадений, `discovery.rs` сохраняет метаданные продолжения.

862: [числа и ячейки](acceptance/native-numeric-evidence-862.md) —
`api/src/pdf_text/layout/`, отдельное представление физической страницы в
`core/src/document/source_nodes.rs`, проекция `retrieval/views.rs` и общий
вывод адресов строк/колонок `core/src/chat_context/tabular.rs`, проверенное
копирование значений `api/src/chat/table_facts.rs`.

861: [выбор целой единицы чтения](acceptance/native-context-selection-861.md) —
`api/src/index_store/sections/documents.rs`, единый бюджет `retrieval/packing.rs`
и неделимые группы доказательств в `core/src/chat_context/`.

860: [упрощение RAG-координатора](acceptance/native-rag-cleanup-860.md) —
`api/src/chat/evidence.rs` владеет допуском и подтверждённой передачей;
`inspection.rs` не хранит состояние. Начальная подготовка и восстановление
используют `api/src/chat/tools/sources/retrieval.rs::initial`.
Список набора и поиск имён используют общий `sources/catalog.rs`; он
применяет и область наборов, и ограничение выбранных файлов. Проекцией
метаданных обоих инструментов владеет `Evidence`.

859: [допуск прочитанных доказательств](acceptance/native-retrieval-859.md) —
`api/src/chat/inspection.rs`, `discovery.rs`, `evidence.rs`, квитанция общего
reader и типизированные блоки в `core/src/chat_context/`. Политика режимов
профилей задаётся `core/src/chat_profiles/mod.rs` и передаётся Svelte сервером.
`api/src/transport.rs::reject` ограничивает дочитывание отвергнутых небольших
HTTP-запросов; квитанции `core/src/receipts.rs` сохраняют причины недоступности
проверки документа отдельно от подтверждённого изменения оригинала.

858: [диагностика поиска](acceptance/native-retrieval-858.md) — отдельная
иерархическая навигация, `api/src/chat/tools/sources/retrieval/packing.rs`,
метаданные обнаружения, проверка чтения и бюджет доказательств в Rust;
`api/src/chat/budget.rs` ограничивает следующую порцию по свободному окну.

857: [точные части Word в чате](acceptance/native-docx-chat-857.md) —
`api/src/chat/tools/sources/parts.rs`, выбор схем `api/src/chat/tools.rs`,
проверка имени/хеша `api/src/documents/parts.rs`.

856: [части DOCX](modules/native-format-conversion.md) — потоковый XML,
`core/src/document/docx/package/`, `api/src/document_media/`, оригиналы и
привязанные к редакции ссылки `api/src/documents/parts.rs`.

855: [семантика DOCX](acceptance/real-corpus-855.md) — шрифты специальных
символов, сетка таблиц, ограниченные Office Math/VML-textbox и точные причины отказа.

854: [исправления по реальному корпусу](acceptance/real-corpus-854.md) —
индексы фильтров Edge, BM25 semijoin, повтор поиска перед ответом, ссылки,
нумерация DOCX и ограниченные повторы OCR.

853: [DOCX и реальный корпус](modules/native-format-conversion.md) —
`core/src/document/docx/`, стили и объединённые ячейки в нативном извлечении.

852: [Режимы поиска в чате](modules/native-chat-retrieval-modes.md) —
`api/src/chat/runner.rs`, `core/src/chat_request/`, общая публикация BM25.

851: [Наблюдение за папками в Rust](modules/native-folder-watch.md) —
`api/src/intake.rs`, `core/src/catalog/intake/watch.rs`, Svelte «Данные и проекты».

846: [Импорт OLM и PST, IMAPS и почтовый индекс](modules/native-mail.md) —
`api/src/mail`, `core/src/catalog/mail` и нативная очередь индексации.

842: [скрепка, настройки и чтение писем](modules/native-settings-attachments.md) —
`api/src/model_registry/`, `core/src/catalog/model_registry.rs`,
`desktop/light/frontend/src/components/ModelSettings.svelte`,
`core/src/document/conversion/mail.rs`.

841: [индексация вложения и обнаружение моделей](modules/native-indexed-attachments.md) —
`core/src/catalog/indexed_attachment.rs`, `api/src/intake.rs`,
`api/src/model_registry/discovery.rs`: оригинал, квитанция очереди и каталоги моделей.

840: [публичные вложения и внешние модели](modules/native-public-attachments.md) —
`api/src/attachments/`, `core/src/chat_attachments/`, `api/src/chat/egress.rs`:
загрузка, восстановление, чтение папок и допуск данных к внешней модели.

839: [публичный вход Rust-чата](modules/native-public-chat.md) —
`api/src/chat/http.rs`, `api/src/chat/view.rs`, `core/src/chat_request/journal.rs`:
допуск, JSON/SSE, ключи повторов по авторизованному клиенту и ссылки в истории.

838: [нативная проверка документов](modules/native-document-review.md) —
`core/src/document_review/`, `api/src/chat/document_review/`: неизменяемый журнал,
передача текста модели, цитаты, зависимости, курсоры и источник в общем чате.

837: [вложения и табличные задания](modules/native-chat-attachments.md) —
`core/src/chat_attachments/`, `core/src/document_tasks/`, `api/src/attachments/`,
`api/src/chat/table_document.rs`: оригиналы, multipart, изображения, точные окна,
неизменяемые строки, журнал решений и общий нативный чат.

836: [память разговора и команды](modules/native-conversation-memory.md) —
`api/src/conversation/`, `core/src/conversation_summary/`, `core/src/chat_commands.rs`:
генерация, durable outbox, приоритет чата, CAS и восстановление без Python callbacks.

835: [общий кандидат чата](modules/native-chat-turn.md) — `api/src/chat/`,
`core/src/chat_request/`, `api/src/documents.rs`: единый JSON/SSE, квитанции,
инструменты, физический PDF и оригиналы. Публичный чат ещё не переключён.

834: [контекст чата](modules/native-chat-context.md) — `core/src/chat_context/`:
упаковка, память/профиль, покрытие разделов, source visibility и delivery trace.

833: [первый индекс](modules/native-index-setup.md) — `api/src/index_bootstrap.rs`,
`core/src/index_bootstrap.rs`, `core/src/lexical_schema.rs`, `vector/src/initialize.rs`.

832: [холодный допуск к API](modules/native-access-bootstrap.md) —
`core/src/access_policy.rs`, `api/src/auth.rs`; прежние Python callbacks удалены.

831: [модельный навигатор набора](modules/native-dataset-reader.md) —
`core/src/catalog/dataset_reader/`, `api/src/dataset_reader.rs`, durable outbox
партии и совместимый локальный caller без Python processing callbacks.

830: [фоновое подключение и запуск партии](modules/native-intake-execution.md) —
`api/src/intake.rs`, `api/src/index_batch/configuration.rs`,
`core/src/index_configuration.rs`. Прежние внутренние FastAPI-адаптеры удалены.

829: [таблицы и нативный HTTP-исполнитель целой партии](modules/native-table-dispatch.md) —
`core/src/tables/`, форматный pipeline, `api/src/index_batch.rs`, атомарные ссылки
Parquet. Проверяется изолированно; общий intake/API ещё не закрыт.

828: [форматные извлекатели](modules/native-format-conversion.md) —
`core/src/document/conversion/`, отдельный `worker_documents`,
`backend/native_document_conversion.py` и привязка Rust launcher.

827: [встроенные расчёты](modules/native-builtin-calculations.md) —
`core/src/workbook_calculation/`; [комплект внешних навыков](modules/skill-runtime-package.md) —
`core/src/skills/runtime.rs`, `tools/skill_python_asset.py` и binding launcher/API.

826: [исходные PDF/Markdown-узлы](modules/native-source-nodes.md), потоковый мост,
сохранение физических страниц, стабильные ID и короткие доказательства.

825: [нативный JSON/JSONL](modules/native-json-sources.md), потоковый C ABI, отказ диагностическим фрагментам и сохранение полей/чисел.

824: [подготовка сырых узлов в Rust](modules/native-node-preparation.md), бюджеты/ID/иерархия и prepared-spool внутри партии.

823: [основной цикл чата в Rust](modules/native-chat-loop.md). Итерации, бюджеты, курсор PDF и финальные попытки; IO/промпты ещё через retained-адаптеры.

822: `core/src/index_batch/` — [координатор партии](modules/native-index-batch.md),
`backend/native_index_batch.py` — совместимый caller конфигурации/HTTP для ручного пути.
[Приёмка](acceptance/native-index-batch-822.md).

821: [автономный host навыков, библиотека и MCP-клиенты/настройки в Rust](modules/native-skills-mcp.md).
Допуск, квитанции, идемпотентность и полные результаты: [нативный исполнитель](modules/native-tool-execution.md).
Оригиналы внешних и загруженных файлов: `core/src/document_source.rs`, [контракт](modules/native-pdf-source.md).

820: [профили, реестр инструментов и файловые инструменты в Rust](modules/native-profiles-files.md).

Кандидат 820: [Edge, BM25, Unicode и локальные/внешние эмбеддинги](modules/native-edge-embeddings.md).
`vector/`, `core/src/embedding_native.rs`, `api/src/embedding_inference.rs`;
[проверка на копии реального корпуса](acceptance/native-edge-embeddings-820.md).

Кандидат 819: [модельный транспорт и интернет-инструменты в Rust](modules/native-web-inference.md).
Поиск и обычное чтение сайтов выполняются нативно; общий цикл чата и API
пока используют адаптер Python. Рабочая установка не переключалась.

Кандидат 818: [реестр моделей, чтение истории и доверенные сети в Rust](modules/native-model-registry.md).
Пять дополнительных HTTP-пар работают без Python backend; холодный запуск
проверяется с явно заданной политикой доступа. Общий runtime ещё нужен.
[Приёмка и ограничения](acceptance/native-registry-history-818.md).

Кандидат 817: [Rust продолжает длинные страницы и читает историю](modules/pdf-cursors-history.md).
Причина ошибки сохраняется до SSE и остаётся после перезапуска.
[Приёмка и незакрытая привязка цитат](acceptance/pdf-cursors-history-817.md).

Кандидат 816: [физические страницы PDF через Rust](modules/native-pdf-source.md),
точные идентификаторы источников и остановка чата при неудачном чтении.
Рабочая установка и backend индекса этим шагом не менялись.

Кандидат 815 сохраняет известный хеш PDF в
`proxy/services/saferag_service.py` и передаёт его панели источника.
[Испытание параллельного чтения и обновления](acceptance/edge-concurrent-pdf-815.md).

Кандидат 814. `core/src/document/embedding_chunks.rs` проверяет финальный
бюджет фрагментов. `core/src/chat_stream.rs` собирает потоковый ответ.
[Граница переноса](modules/python-exit.md).

Кандидат 813: `core/src/index_sources.rs` читает ручную очередь и находит
зарегистрированные файлы; `core/src/files.rs` получает их отпечатки.
[Контракт партии](modules/auto-parse-resume.md).

Кандидат 812: `api/src/index_store/cache.rs` читает кеш векторов Qdrant,
а `core/src/index_cache.rs` передаёт его сборщику точек без JSON в Python.
[Контракт партии](modules/auto-parse-resume.md).

Кандидат 811: `core/src/legacy_sparse.rs` сохраняет контракт TF-вектора Qdrant.
Он вызывается напрямую сборщиком точек и через нативный адаптер запросов.
BM25 с корпусной статистикой описан [отдельно](modules/bm25-index.md).

Кандидат 810: `core/src/index_points.rs` управляет порциями эмбеддингов,
проверяет векторы и собирает JSONL для Rust-публикации. Адаптер подключения
и выдачи подготовленных фрагментов находится в `backend/native_core.py`.
[Контракт партии](modules/auto-parse-resume.md).

Кандидат 804: Rust заменяет лексическую копию файла, читает родительские
разделы Qdrant и обрабатывает вход и результаты инструментов в харнессе.
[Лексический индекс](modules/bm25-index.md), [чтение разделов](modules/chat-section-context.md),
[цикл инструментов](modules/native-harness-loop.md).

Кандидат 803: `api/src/intake.rs` и `core/src/catalog/intake.rs` ведут фоновый
импорт и сохраняют прогресс после остановки. `api/src/index_store.rs` публикует
и удаляет точки в локальном Qdrant. `api/src/pdf_text.rs` обслуживает текстовый
слой PDF в отдельном процессе.
[Импорт](modules/auto-parse-resume.md), [извлечение](modules/document-extraction.md).

Кандидат 795: `api/src/frontend.rs` проверяет и раздаёт собранные файлы
чата, а запускатель передаёт адрес прежних экранов.
[Контракт](modules/svelte-desktop.md).

Кандидат 794: `api/src/auth_keys.rs` и `core/src/auth_keys.rs`
обслуживают семь маршрутов реестра ключей и привязки устройств.
[Контракт](modules/native-api.md).

Кандидат 793: `api/src/auth.rs` авторизует прямой локальный вызов нативных
маршрутов. `core/src/auth.rs` читает прежний реестр ключей без записи.
[Контракт](modules/native-api.md).

Кандидат 792: `runtime/src/cli` владеет CLI и внешним MCP stdio.
`tools/build_light_package.py` выбирает Rust-команду и совместимые точки входа.
[Контракт](modules/native-runtime.md).

Кандидат 791: `core/src/document/workbook_index.rs` определяет размеры
и готовит значения строк XLSX для существующей структурной индексации.
`backend/workbook_index.py` связывает классификатор и Parquet с ядром.
[Контракт](modules/document-tasks.md).

Кандидат 790: `core/src/document/workbook.rs` читает снимки XLSX/XLSM для
вложений, дочитывания и расчётов в чате. Граница с совместимым адаптером
описана в [контракте таблиц](modules/document-tasks.md).

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

Безопасный старт коллекции: [нативный bootstrap](modules/native-index-setup.md) (lease/exact count/
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

[Лаборатория поиска, синхронизация папок и сравнение с Msty](acceptance/retrieval-lab-805.md).
`backend/bm25_store.py` — postings и точная оценка при запросе;
`backend/sparse_index.py` — миграция и журнал областей изменений;
`backend/bm25_hybrid.py` — объединение с dense, `sparse_legacy.py` — TF-откат.
Рабочий BM25, миграция и обновление без LLM: [контракт](modules/bm25-index.md).
Версия словаря и короткие обозначения: `backend/inference/lexical_tokens.py`.
Уведомление об отказе с явным переходом: `sovushka/components/chat_failure_notice.py`.
Нагрузочный стенд без моделей: `tools/benchmark_sparse_index.py`.
Отдельный кандидат BM25 и чтения разделов: [контракт испытаний](modules/retrieval-candidate.md).
Чтение родителей в основном чате и бюджет доказательств: [контракт](modules/chat-section-context.md).
`pdf_page_context_service.py` читает соседние страницы из индекса.
`pdf_table_continuation_service.py` проверяет продолжение таблицы по оригиналу.
`word_evidence_service.py` возвращает адресные абзацы и строки таблиц DOCX.
Сравнение полного и минимального поиска, модельного пакета и локальных судей:
[стенд 764](modules/rag-pipeline-trial.md). `lexical_evidence_service.py` сверяет
FTS ID с Qdrant; `core/src/chat_context/coverage.rs` формирует модельную
метаинформацию о полноте реально переданных индексированных разделов.

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
Навыки и вычисления: `core/src/skills/`, протокол `les.skills.v1`;
`installed_skill_service.py`, `skill_compute_service.py` — совместимые кодеки.
MCP-клиенты/настройки: `api/src/mcp/`, SQLite: `core/src/mcp_registry.rs`.
Компактный каталог выбранных расширений: `proxy/services/extension_tool_service.py`.
Наблюдение за службами: `backend/light_health_monitor.py`.

Точные таблицы: `proxy/services/tabular_document_service.py` (координаты, формулы,
снимок источника). Задания по строкам: `document_task_store.py` (атомарные пакеты,
область, возобновление), `table_document_tool.py` (инструмент текущего диалога).
Контракт: [Работа с таблицами](modules/document-tasks.md).
`workbook_total_service.py` считает по указанным диапазонам оригинала Excel
и возвращает отдельные доказательства по листам.
Общая очередь выбранных документов/разделов: `document_review_tool.py`,
`review_task_store.py`, `review_source_reader.py`; [контракт](modules/document-review.md).
`document_task_database.py` — общий транспорт SQLite; `table_window_service.py`
— адресные окна и чтение длинных ячеек; `bound_document_tools.py` — обязательный
контекст привязанных инструментов и их место в shortlist.
`table_retrieval_service.py` читает строки оригиналов при обычном запросе по базе.
[Покрытие и ссылки](modules/chat-section-context.md).
`native_tool_message_service.py` — адаптер канонических assistant/tool к native
chat: объект arguments и имя tool_name без изменения сохранённого диалога.

[Карта кода](CODE_MAP.md) · [Архитектура](LIGHT_ARCHITECTURE.md)
