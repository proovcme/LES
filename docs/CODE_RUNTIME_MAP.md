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
| Python-файлов под git | 555 |
| Строк Python | 155915 |
| PRODUCT_REACHABLE | 368 |
| RUNTIME_SUPPORT | 5 |
| TEST_OR_TOOL_ONLY | 147 |
| DORMANT_CANDIDATE | 35 |
| Зарегистрированных API-маршрутов | 292 |
| Ошибок разбора | 0 |

## Крупнейшие продуктовые модули

| Файл | Строк | Прямых потребителей |
| --- | --- | --- |
| sovushka/styles.py | 3264 | 3 |
| proxy/services/dataset_memory_service.py | 2284 | 9 |
| mlx_host.py | 2216 | 0 |
| sovushka/pages/chat.py | 2200 | 2 |
| proxy/services/chat_evidence_application_service.py | 1993 | 7 |
| proxy/services/project_pdf_table_service.py | 1653 | 2 |
| sovushka/pages/diag.py | 1606 | 1 |
| proxy/services/checklist_review_service.py | 1518 | 1 |
| proxy/routers/mail.py | 1502 | 4 |
| proxy/services/cad_bim_graph.py | 1425 | 2 |
| proxy/services/retrieval_service.py | 1400 | 10 |
| proxy/services/tool_harness_service.py | 1396 | 14 |
| proxy/services/context_memory_service.py | 1175 | 8 |
| sovushka/pages/samovar.py | 1143 | 3 |
| backend/document_router.py | 1124 | 6 |
| tools/build_rag_contract_sibling.py | 1114 | 2 |
| tools/reindex_datasets_guarded.py | 1016 | 1 |
| proxy/services/drawing_manifest_service.py | 1012 | 3 |
| backend/document_catalog.py | 977 | 2 |
| sovushka/components/header.py | 968 | 2 |
| backend/parquet_writer.py | 965 | 3 |
| proxy/services/chat_profile_service.py | 958 | 7 |
| proxy/routers/runtime.py | 920 | 3 |
| proxy/services/project_pdf_extract_service.py | 915 | 4 |
| tools/les_runtime_control.py | 913 | 4 |
| proxy/services/mail_registry_service.py | 903 | 6 |
| proxy/services/document_explorer_service.py | 866 | 4 |
| proxy/services/project_document_registry_service.py | 853 | 3 |
| proxy/services/notebook_study_service.py | 833 | 3 |
| backend/mail_profile.py | 825 | 5 |

## Чат Light: фактические потребители

### `proxy/routers/chat.py`

Статус: `PRODUCT_REACHABLE`; строк: 466.

| Импортируемый символ | Потребители |
| --- | --- |
| `ChatRouterState` | `proxy/app.py` |
| `_active_dispatcher_reindex_jobs` | `proxy/routers/workspace_memory.py` |
| `_generation_token_budget` | `tests/test_source_excerpts.py` |
| `_llm_runtime` | `proxy/routers/settings.py` |
| `_local_context_budget` | `tests/test_source_excerpts.py` |
| `clean_visible_text` | `tests/test_source_excerpts.py` |
| `ensure_chat_history_schema` | `proxy/app.py`<br>`proxy/routers/chat_history.py` |
| `get_chat_state` | `proxy/routers/workspace_memory.py`<br>`proxy/services/source_adapters.py` |
| `router` | `proxy/app.py`<br>`tests/test_light_router_boundary.py` |
| `set_chat_state` | `proxy/app.py` |
| `source_excerpts` | `tests/test_source_excerpts.py` |

## Кандидаты на проверку

| Файл | Строк | Почему только кандидат |
| --- | --- | --- |
| backend/pdf_layout.py | 254 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| backend/rules_extractor.py | 127 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/active_state_service.py | 129 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/agent_router_service.py | 467 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/asbuilt_chat_service.py | 122 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/asbuilt_intake_service.py | 424 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/asbuilt_ocr.py | 159 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/candidate_selection_service.py | 136 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/checklist_template_importer.py | 450 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/clarification_service.py | 225 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/construction_harness_service.py | 477 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/document_object_model.py | 79 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/document_outline_service.py | 108 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/fsem_machinist_service.py | 175 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/glossary_chat_service.py | 90 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/harvest_service.py | 213 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/help_chat_service.py | 97 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/kac_web_service.py | 180 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/les_md_chat_service.py | 66 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/nr_sp_service.py | 93 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/office_passport_service.py | 175 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/parent_card_hydration_service.py | 172 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/preset_chat_service.py | 64 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/quantity_trace_service.py | 145 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/reconcile_chat_service.py | 150 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/resource_cost_service.py | 801 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/scoped_rag_builder.py | 111 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/sovushka_tone.py | 49 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/spreadsheet_object_model.py | 153 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/stesnennost_service.py | 116 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/table_detect.py | 86 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/table_sql_service.py | 222 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/tool_provider_projection_service.py | 23 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/unified_construction_harness_service.py | 959 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |
| proxy/services/update_service.py | 1082 | Нет доказанного статического пути; проверить dynamic/subprocess/external use |

## Ограничения

- Карта видит обычные Python-импорты и декораторы `APIRouter`, но не доказывает фактическую частоту вызова.
- Строковые импорты, plugin discovery, subprocess и внешние entrypoint требуют ручной проверки.
- Удаление возможно только после отдельного поиска потребителей, теста и проверки установленного Windows runtime.
