# Rust HTTP API · builds 776–779

`api/` — Axum 0.8.9 executable `les-api`. Режим Tauri `--native-api` также
включает Rust-запускатель. Внешний loopback-порт API принадлежит Rust; прежний
FastAPI слушает отдельный внутренний порт. Все процессы принадлежат одному
runtime, наблюдаются и завершаются вместе. Отсутствующий binary не включает
скрытый откат. Без флага остаётся прежний режим кандидата 775.

В 779 нативно обслуживаются **22 из 267** зафиксированных method/path пар.
776 перенёс шесть, 777 — двадцать из прежних 265.

Кандидат 778 добавляет GET/POST `/api/workspace/agent-state`: **22 из 267**,
старые 265 пар без изменений. Владелец `rust.agent-state`, JSON до 2 МиБ,
настроенный абсолютный `--state`; модель и тело запроса корень не выбирают.
[Память/файлы агента](agent-workspace.md). Остальные 245 пар пока прежние.
779 расширяет только agent-state: delete/export и необязательные параметры.

| Метод | Путь |
| --- | --- |
| GET, POST | `/api/workspace/sessions` |
| GET, PATCH | `/api/workspace/sessions/{session_id}` |
| GET, PUT | `/api/workspace/sessions/{session_id}/draft` |
| GET, POST | `/api/projects` |
| GET, PATCH, DELETE | `/api/projects/{project_id}` |
| GET, POST, DELETE | `/api/projects/{project_id}/links` |
| GET | `/api/workspace/memory` |
| PATCH, DELETE | `/api/workspace/memory/{note_id}` |
| GET, PATCH | `/api/workspace/memory/context/{session_id}` |
| POST | `/api/workspace/memory/context/{session_id}/forget` |
| GET, POST | `/api/workspace/agent-state` |

Они напрямую вызывают [Rust-владелец диалогов](native-workspace.md) и
[каталог проектов/заметок/контекста](native-catalog.md).
Блокирующие операции выполняются вне HTTP-потоков, одновременно не более 16;
клиентский disconnect не освобождает слот до окончания транзакции. JSON ограничен
1 МиБ, ожидание слота/SQLite ограничено. Ошибки сохранения имеют понятное действие,
в служебный журнал попадают операция, стабильный код и номер SQLite, без SQL,
путей, ключей и текста пользователя.

Авторизация пока принадлежит **существующим require_user/require_admin**: нативный
запрос проверяется через внутренний `/api/light/native-authorization`, без кэша и
обхода проверки. Изменение проектов требует require_admin через
`/api/light/native-admin-authorization`. Оба служебных пути исключены из OpenAPI. Host/Origin
ограничены локальными адресами; отказ авторизации не создаёт БД. CORS переносится
также на ошибки, OPTIONS остаётся в существующем middleware.

Остальные пути явно проксируются прежнему API. Загрузка и ответ идут потоком,
без накопления всего тела; SSE/UTF-8 передаются без перекодирования, отмена
закрывает upstream. Адрес upstream фиксирован в конфигурации, произвольный URL из
запроса не используется. Сохраняется проверенный публичный Host для ссылок на
документы/превью, локальные абсолютные редиректы становятся относительными.
Hop-by-hop заголовки удаляются. `x-les-handler` различает `rust.workspace`,
`rust.catalog`, `rust.agent-state` и `legacy.proxy`, поэтому проксирование нельзя
выдать за перенос реализации.

`--contract` описывает реальное владение. Сборка: `tools/build_native_api.py
--offline --output NEW_DIR`, пакетирование `--native-api FILE` требует
`--native-runtime FILE`. При staging проверяются SHA binary, digest текущих
core/API/version/lock и remap. В локальном патче 779 стандартный ярлык явно
включает `--native-api --svelte`; прямой EXE без флагов сохраняет classic.
Полный установщик не пересобран;
совместимость чата/harness и всех остальных 245 пар не равна наличию прокси.

Поведенческий gate `tests/test_light_native_api.py`: доступ, CORS, CAS-конкуренция,
неблокирующий отказ занятой БД, побайтовый SSE, отмена, загрузка 2 МиБ, ссылки и
редиректы. Используются отдельный тестовый upstream и собственная временная БД,
модели/документы владельца не нужны. Baseline OpenAPI проверяется отдельно:
`tools/native_api_contract.py --check`. [Runtime](native-runtime.md).
