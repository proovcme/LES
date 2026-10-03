# LES Light 0.1.0: offline checks and local packaging only.
# Public release and installation are separate, explicitly authorized actions.
.PHONY: help version-sync verify test test-mail test-tauri public-check package validate-package

PYTEST_BASETEMP ?= .test-tmp
PKGS := backend proxy sovushka tools sovushka_ng.py proxy_server.py mlx_host.py

LIGHT_TESTS := $(wildcard tests/test_light_*.py)
LIGHT_TESTS += tests/test_dataset_watch.py tests/test_dataset_deletion_service.py
LIGHT_TESTS += tests/test_sovushka_uikit.py tests/test_forest_visualizer.py tests/test_sovushka_data_workspace.py
LIGHT_TESTS += tests/test_answer_render_v16.py tests/test_public_error_service.py tests/test_workspace_context.py
LIGHT_TESTS += tests/test_converter_email.py tests/test_runtime_admission.py
LIGHT_TESTS += tests/test_model_connection_embeddings_integration.py tests/test_qdrant_adapter_parse.py tests/test_parse_resume.py tests/test_chunking_w2.py
LIGHT_CORE_TESTS := tests/test_answer_contract_service.py tests/test_chat_session_service.py tests/test_chat_workspace_ui.py tests/test_code_runtime_map.py tests/test_context_governor_service.py tests/test_datasets_router.py tests/test_document_explorer_service.py tests/test_documentation_contract.py tests/test_evidence_contract.py tests/test_evidence_packet_service.py tests/test_mail_router.py tests/test_memory_api.py tests/test_memory_core.py tests/test_model_capability_service.py tests/test_model_connection_chat_integration.py tests/test_model_connection_registry_service.py tests/test_model_connection_resolver_service.py tests/test_model_connection_security_service.py tests/test_model_connections_router.py tests/test_openai_compatible_transport_service.py tests/test_product_edition.py tests/test_rag_config.py tests/test_rag_rrf_readiness.py tests/test_retrieval_service.py tests/test_saferag_service.py tests/test_software_versions.py tests/test_source_excerpts.py tests/test_sovushka_model_connections.py tests/test_web_search_service.py tests/test_workspace_memory.py tests/test_workspace_memory_router.py tests/test_workspace_router.py
CURRENT_TESTS := $(sort $(LIGHT_TESTS) $(LIGHT_CORE_TESTS))
MAIL_TESTS := tests/test_chat_mail_query.py tests/test_converter_email.py tests/test_ezhik_imap_smoke.py tests/test_mail_ingest.py tests/test_mail_profile.py tests/test_mail_push_service.py tests/test_mail_query_service.py tests/test_mail_registry_service.py tests/test_mail_router.py tests/test_mail_threads.py tests/test_outlook_mail_poller.py

help:
	@echo "make verify           — синтаксис и сбор тестов LES Light без сервисов"
	@echo "make test             — поведенческий gate LES Light"
	@echo "make test-mail        — отдельные офлайн-проверки почты"
	@echo "make test-tauri       — проверка desktop shell"
	@echo "make public-check     — аудит перед будущей публикацией"
	@echo "make package          — автономный комплект, требуются DESKTOP_EXE и QDRANT_EXE"
	@echo "make validate-package — проверка манифеста, требуется PACKAGE_DIR"

version-sync:
	uv run python tools/sync_version_contract.py

verify:
	uv run python tools/sync_version_contract.py --check
	uv run python tools/code_runtime_map.py --check
	uv run python -m compileall -q $(PKGS)
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/verify --collect-only -q $(CURRENT_TESTS)

test:
	uv run python tools/code_runtime_map.py --check
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/test -q --durations=20 $(CURRENT_TESTS)

test-mail:
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/mail -q --durations=15 $(MAIL_TESTS)

test-tauri:
	cargo check --manifest-path desktop/light/src-tauri/Cargo.toml

public-check:
	uv run python tools/publication_check.py

package:
	@test -n "$(DESKTOP_EXE)" -a -n "$(QDRANT_EXE)" -a -n "$(PACKAGE_DIR)" || (echo "Set DESKTOP_EXE, QDRANT_EXE and PACKAGE_DIR" && exit 2)
	uv run python tools/build_light_package.py --output "$(PACKAGE_DIR)" --desktop-exe "$(DESKTOP_EXE)" --qdrant "$(QDRANT_EXE)"

validate-package:
	@test -n "$(PACKAGE_DIR)" || (echo "Set PACKAGE_DIR" && exit 2)
	pwsh -NoProfile -File installers/windows/light/package.ps1 -Mode Validate -Source "$(PACKAGE_DIR)"
