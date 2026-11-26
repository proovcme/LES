# Карта исполняемого кода ЛЕС

> Сгенерировано `tools/code_runtime_map.py`. Не редактировать вручную.

Это консервативная статическая карта импортов и зарегистрированных FastAPI-маршрутов. Статус `DORMANT_CANDIDATE` означает только отсутствие доказанного пути от продуктовых entrypoint, явного runtime-helper или теста; он **не является доказательством мёртвого кода**.

Полный построчный inventory находится в `docs/generated/code_runtime_map.json`.

## Статусы

| Статус | Что доказано |
| --- | --- |
| PRODUCT_REACHABLE | Есть статический путь от боевой точки входа |
| RUNTIME_SUPPORT | Явно перечислен как отдельный helper Windows runtime |
| TEST_OR_TOOL_ONLY | Тест, служебный скрипт или достигается только из такого кода |
| DORMANT_CANDIDATE | Статический потребитель не найден; требуется ручная проверка |

## Сводка

| Метрика | Значение |
| --- | --- |
| Python-файлов под git | 715 |
| Строк Python | 169066 |
| PRODUCT_REACHABLE | 414 |
| RUNTIME_SUPPORT | 5 |
| TEST_OR_TOOL_ONLY | 296 |
| DORMANT_CANDIDATE | 0 |
| Зарегистрированных API-маршрутов | 285 |
| Ошибок разбора | 0 |

## Крупнейшие продуктовые модули

| Файл | Строк | Прямых потребителей |
| --- | --- | --- |
| sovushka/styles.py | 3264 | 3 |
| sovushka/pages/chat.py | 2196 | 2 |
| proxy/services/chat_evidence_application_service.py | 2122 | 7 |
| proxy/services/dataset_memory_service.py | 1811 | 10 |
| proxy/services/project_pdf_table_service.py | 1653 | 2 |
| sovushka/pages/diag.py | 1606 | 1 |
| proxy/services/checklist_review_service.py | 1517 | 1 |
| proxy/routers/mail.py | 1502 | 4 |
| proxy/services/retrieval_service.py | 1440 | 11 |
| proxy/services/cad_bim_graph.py | 1425 | 3 |
| proxy/services/tool_harness_service.py | 1275 | 25 |
| proxy/services/context_memory_service.py | 1175 | 8 |
| sovushka/pages/samovar.py | 1143 | 3 |
| backend/document_router.py | 1118 | 10 |
| tools/build_rag_contract_sibling.py | 1116 | 2 |
| backend/document_catalog.py | 1068 | 19 |
| tools/reindex_datasets_guarded.py | 1016 | 1 |
| proxy/services/drawing_manifest_service.py | 1012 | 3 |
| sovushka/components/header.py | 1003 | 2 |
| proxy/routers/runtime.py | 922 | 3 |
| proxy/services/project_pdf_extract_service.py | 915 | 4 |
| proxy/services/document_explorer_service.py | 904 | 5 |
| proxy/services/mail_registry_service.py | 903 | 6 |
| tools/les_runtime_control.py | 902 | 4 |
| proxy/services/project_document_registry_service.py | 853 | 3 |
| backend/parquet_writer.py | 834 | 6 |
| proxy/services/notebook_study_service.py | 833 | 3 |
| backend/mail_profile.py | 825 | 5 |
| proxy/services/chat_request_service.py | 800 | 5 |
| backend/pst_reader.py | 790 | 1 |

## Чат Light: фактические потребители

### `proxy/routers/chat.py`

Статус: `PRODUCT_REACHABLE`; строк: 469.

| Импортируемый символ | Потребители |
| --- | --- |
| `ChatRouterState` | `proxy/app.py` |
| `_active_dispatcher_reindex_jobs` | `proxy/routers/workspace_memory.py` |
| `_generation_token_budget` | `tests/test_source_excerpts.py` |
| `_llm_runtime` | `proxy/routers/settings.py` |
| `_local_context_budget` | `tests/test_source_excerpts.py` |
| `clean_visible_text` | `tests/test_source_excerpts.py` |
| `ensure_chat_history_schema` | `proxy/app.py` |
| `get_chat_state` | `proxy/routers/workspace_memory.py`<br>`proxy/services/source_adapters.py` |
| `router` | `proxy/app.py`<br>`tests/test_light_router_boundary.py` |
| `set_chat_state` | `proxy/app.py` |
| `source_excerpts` | `tests/test_source_excerpts.py` |

## Кандидаты на проверку

Кандидатов нет.

## Ограничения

- Карта видит обычные Python-импорты и декораторы `APIRouter`, но не доказывает фактическую частоту вызова.
- Строковые импорты, plugin discovery, subprocess и внешние entrypoint требуют ручной проверки.
- Удаление возможно только после отдельного поиска потребителей, теста и проверки установленного Windows runtime.
