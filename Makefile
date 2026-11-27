# LES RAG development checks and local packaging.
.PHONY: help version-sync verify-fast verify pre-push test test-mail test-tauri test-core test-runtime test-api frontend test-frontend public-check package validate-package

PYTEST_BASETEMP ?= .test-tmp
PKGS := backend proxy tools

LIGHT_TESTS := $(wildcard tests/test_light_*.py)
LIGHT_TESTS += tests/test_dataset_deletion_service.py
LIGHT_TESTS += tests/test_forest_visualizer.py
LIGHT_TESTS += tests/test_public_error_service.py
LIGHT_TESTS += tests/test_converter_email.py tests/test_runtime_admission.py
LIGHT_TESTS += tests/test_model_connection_embeddings_integration.py tests/test_qdrant_adapter_parse.py tests/test_parse_resume.py tests/test_chunking_w2.py
LIGHT_CORE_TESTS := tests/test_answer_contract_service.py tests/test_chat_session_service.py tests/test_code_runtime_map.py tests/test_context_governor_service.py tests/test_document_explorer_service.py tests/test_documentation_contract.py tests/test_evidence_contract.py tests/test_evidence_packet_service.py tests/test_memory_core.py tests/test_model_capability_service.py tests/test_model_connection_registry_service.py tests/test_model_connection_resolver_service.py tests/test_model_connection_security_service.py tests/test_openai_compatible_transport_service.py tests/test_product_edition.py tests/test_rag_config.py tests/test_rag_rrf_readiness.py tests/test_retrieval_service.py tests/test_saferag_service.py tests/test_software_versions.py tests/test_workspace_memory.py
CURRENT_TESTS := $(sort $(LIGHT_TESTS) $(LIGHT_CORE_TESTS))
MAIL_TESTS := tests/test_chat_mail_query.py tests/test_converter_email.py tests/test_ezhik_imap_smoke.py tests/test_mail_ingest.py tests/test_mail_profile.py tests/test_mail_push_service.py tests/test_mail_query_service.py tests/test_mail_registry_service.py tests/test_mail_threads.py tests/test_outlook_mail_poller.py
PRE_PUSH_TESTS := tests/test_documentation_contract.py tests/test_light_git_gates.py tests/test_light_bootstrap.py
PRE_PUSH_TESTS += tests/test_rag_config.py tests/test_rag_rrf_readiness.py tests/test_evidence_contract.py

help:
	@echo "make pre-push         — быстрые проверки и офлайн-тесты перед пушем"
	@echo "make verify           — синтаксис и сбор тестов LES Light без сервисов"
	@echo "make test             — поведенческий gate LES Light"
	@echo "make test-mail        — отдельные офлайн-проверки почты"
	@echo "make test-tauri       — проверка desktop shell"
	@echo "make public-check     — аудит перед будущей публикацией"
	@echo "make package          — автономный комплект, требуются DESKTOP_EXE и QDRANT_EXE"
	@echo "make validate-package — проверка манифеста, требуется PACKAGE_DIR"

version-sync:
	uv run python tools/sync_version_contract.py

verify-fast:
	uv run python tools/sync_version_contract.py --check
	uv run python tools/code_runtime_map.py --check
	uv run python -m compileall -q $(PKGS)

verify: verify-fast
	uv run python tools/native_api_contract.py --check
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/verify --collect-only -q $(CURRENT_TESTS)

pre-push: verify-fast public-check
	uv run python tools/documentation_contract.py
	uv run python -m tools.light_public_docs --check
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/pre-push -q $(PRE_PUSH_TESTS)

test:
	uv run python tools/build_light_frontend.py --check
	uv run python tools/code_runtime_map.py --check
	uv run --frozen --extra mcp python -m pytest --basetemp=$(PYTEST_BASETEMP)/test -q --durations=20 $(CURRENT_TESTS)

test-mail:
	uv run python -m pytest --basetemp=$(PYTEST_BASETEMP)/mail -q --durations=15 $(MAIL_TESTS)

test-tauri:
	cargo check --manifest-path desktop/light/src-tauri/Cargo.toml

test-core:
	cargo fmt --manifest-path core/Cargo.toml --check
	cargo test --locked --manifest-path core/Cargo.toml

test-runtime:
	cargo fmt --manifest-path runtime/Cargo.toml --check
	cargo test --locked --manifest-path runtime/Cargo.toml

test-api:
	cargo fmt --manifest-path api/Cargo.toml --check
	cargo test --locked --manifest-path api/Cargo.toml

frontend:
	uv run python tools/build_light_frontend.py

test-frontend:
	uv run python tools/build_light_frontend.py --test

public-check:
	uv run python tools/publication_check.py

package:
	@test -n "$(DESKTOP_EXE)" -a -n "$(QDRANT_EXE)" -a -n "$(PACKAGE_DIR)" || (echo "Set DESKTOP_EXE, QDRANT_EXE and PACKAGE_DIR" && exit 2)
	uv run python tools/build_light_package.py --output "$(PACKAGE_DIR)" --desktop-exe "$(DESKTOP_EXE)" --qdrant "$(QDRANT_EXE)" $(if $(ENGINEERING_READERS),--engineering-readers "$(ENGINEERING_READERS)")

validate-package:
	@test -n "$(PACKAGE_DIR)" || (echo "Set PACKAGE_DIR" && exit 2)
	pwsh -NoProfile -File installers/windows/light/package.ps1 -Mode Validate -Source "$(PACKAGE_DIR)"
