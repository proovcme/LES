# Карта исполняемого кода ЛЕС

> Сгенерировано `tools/code_runtime_map.py`. Не редактировать вручную.

Это статическая карта отдельных Python-инструментов и тестовых зависимостей. Действующие HTTP-маршруты Windows проверяются запуском Rust API и записываются в generated/native-api-routes.json. Статус `DORMANT_CANDIDATE` показывает отсутствие найденного пути от точки входа или теста; он **не является доказательством мёртвого кода**.

Полный построчный inventory находится в `docs/generated/code_runtime_map.json`.

## Статусы

| Статус | Что доказано |
| --- | --- |
| PRODUCT_REACHABLE | Есть путь от указанной точки входа Python |
| TEST_OR_TOOL_ONLY | Тест, служебный скрипт или достигается только из такого кода |
| DORMANT_CANDIDATE | Статический потребитель не найден; требуется ручная проверка |

## Сводка

| Метрика | Значение |
| --- | --- |
| Python-файлов под git | 525 |
| Строк Python | 112810 |
| PRODUCT_REACHABLE | 101 |
| TEST_OR_TOOL_ONLY | 424 |
| DORMANT_CANDIDATE | 0 |
| Зарегистрированных API-маршрутов | 9 |
| Ошибок разбора | 0 |

## Крупнейшие модули Python-совместимости

| Файл | Строк | Прямых потребителей |
| --- | --- | --- |
| proxy/services/dataset_memory_service.py | 1811 | 6 |
| proxy/services/project_pdf_table_service.py | 1653 | 2 |
| backend/document_router.py | 1118 | 10 |
| backend/document_catalog.py | 1068 | 19 |
| proxy/services/drawing_manifest_service.py | 1012 | 3 |
| proxy/services/project_pdf_extract_service.py | 915 | 4 |
| proxy/services/mail_registry_service.py | 903 | 5 |
| proxy/services/project_document_registry_service.py | 853 | 2 |
| backend/parquet_writer.py | 834 | 5 |
| backend/mail_profile.py | 825 | 5 |
| proxy/services/openai_compatible_transport_service.py | 784 | 10 |
| backend/qdrant_ingestion.py | 748 | 12 |
| backend/converter.py | 647 | 15 |
| proxy/services/lexical_index_service.py | 631 | 31 |
| proxy/services/electrical_schematic_service.py | 622 | 1 |
| backend/qdrant_support.py | 568 | 27 |
| proxy/services/project_table_registry_service.py | 564 | 2 |
| proxy/services/pd_rd_manifest_service.py | 559 | 4 |
| backend/qdrant_nodes.py | 536 | 11 |
| proxy/services/electrical_materials_service.py | 528 | 1 |
| backend/mail_ingest.py | 510 | 8 |
| proxy/services/electrical_evidence_summary_service.py | 484 | 1 |
| backend/mail_threads.py | 483 | 4 |
| proxy/services/runtime_admission.py | 451 | 12 |
| proxy/services/model_capability_service.py | 440 | 5 |
| backend/rag_config.py | 419 | 58 |
| proxy/services/pdf_contour_service.py | 408 | 4 |
| proxy/services/kot_service.py | 396 | 7 |
| lemonade_host.py | 381 | 0 |
| backend/embedding_client.py | 375 | 11 |

## Чат Light: фактические потребители

### `proxy/services/retrieval_service.py`

Статус: `TEST_OR_TOOL_ONLY`; строк: 1440.

| Импортируемый символ | Потребители |
| --- | --- |
| `CURRENT` | `tests/test_retrieval_service.py` |
| `classify_query` | `proxy/routers/dataset_search.py`<br>`tests/test_retrieval_service.py` |
| `expand_retrieval_query` | `tests/test_retrieval_service.py` |
| `hybrid_backend` | `proxy/services/table_appendix_service.py`<br>`tests/test_retrieval_service.py` |
| `infer_dataset_filter` | `tests/test_retrieval_service.py` |
| `required_reranker_policy` | `tests/test_retrieval_service.py` |
| `resolve_dataset_ids` | `proxy/routers/dataset_search.py`<br>`tests/test_retrieval_service.py` |
| `retrieve_chat_chunks` | `proxy/routers/dataset_search.py`<br>`tests/test_light_index_recovery.py`<br>`tests/test_light_rerank_audit.py`<br>`tests/test_retrieval_service.py`<br>`tools/evaluate_rag_pipeline.py` |

## Кандидаты на проверку

Кандидатов нет.

## Ограничения

- Карта видит обычные Python-импорты и декораторы `APIRouter`, но не доказывает фактическую частоту вызова.
- Строковые импорты, plugin discovery, subprocess и внешние entrypoint требуют ручной проверки.
- Удаление возможно только после отдельного поиска потребителей, теста и проверки установленного Windows runtime.
