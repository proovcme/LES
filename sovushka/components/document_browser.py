"""Document browser state, read operations and user actions."""
from __future__ import annotations

import asyncio
import inspect
import json
from urllib.parse import quote, urlencode
from nicegui import context, ui
from sovushka.state import api_get, api_post, api_post_file, last_api_error_text
from sovushka.components.document_presentation import _dataset_title, _file_sort_key, _schedule
from sovushka.components.document_views import DocumentViews


class DocumentBrowser:
    def __init__(self, state: dict, refs: dict, *, surface: str, can_manage: bool,
                 initial_mode: str, initial_note: str, initial_dataset: str):
        self.state = state
        self.refs = refs
        self.surface = surface
        self.can_manage = can_manage
        self.initial_mode = initial_mode
        self.initial_note = initial_note
        self.initial_dataset = initial_dataset
        self._selection_revision = 0
        self._content_revision = 0
        self._list_revision = 0
        self.view = DocumentViews(self)

    def _is_system_dataset(self, row: dict | None = None) -> bool:
        target = row or self._selected_dataset_row()
        return str((target or {}).get("dataset_scope") or "user") == "system"

    def _selected_dataset_row(self) -> dict:
        dataset_id = str(self.state.get("selected_dataset") or "")
        for row in self.state.get("datasets") or []:
            if str(row.get("id") or "") == dataset_id:
                return row
        return {}

    def _document_by_file_name(self, file_name: str) -> dict:
        file_name = str(file_name or "")
        basename = file_name.rsplit("/", 1)[-1]
        for row in self.state.get("documents") or []:
            row_name = str(row.get("file_name") or "")
            if row_name == file_name or row_name == basename or row_name.endswith("/" + basename):
                return row
        return {}

    def _source_map_files(self, memory: dict, project_pdf: dict) -> list[dict]:
        files = [item for item in (project_pdf.get("files") or []) if isinstance(item, dict)]
        if not files and isinstance(memory.get("project_pdf_extract"), dict):
            files = [item for item in (memory.get("project_pdf_extract", {}).get("files") or []) if isinstance(item, dict)]
        if files:
            return files
        result: list[dict] = []
        for row in self.state.get("documents") or []:
            file_name = str(row.get("file_name") or "")
            result.append(
                {
                    "file_name": file_name,
                    "doc_id": str(row.get("id") or ""),
                    "doc_role": str(row.get("doc_type") or row.get("content_type") or "документ"),
                    "discipline": str(row.get("domain") or ""),
                    "source_path": str(row.get("source_path") or ""),
                    "status": "ok" if str(row.get("status") or "").upper() == "INDEXED" else str(row.get("status") or ""),
                    "layers": [str(row.get("content_type") or "")] if row.get("content_type") else [],
                }
            )
        return sorted(result, key=_file_sort_key)

    def _composition_files(self, memory: dict, project_pdf: dict) -> list[dict]:
        """Full dataset inventory enriched by LIST metadata when available."""
        list_files = self._source_map_files(memory, project_pdf)
        list_by_name = {str(item.get("file_name") or ""): item for item in list_files}
        result: list[dict] = []
        seen: set[str] = set()
        for row in self.state.get("documents") or []:
            file_name = str(row.get("file_name") or "")
            enriched = {
                "file_name": file_name,
                "doc_id": str(row.get("id") or ""),
                "status": str(row.get("status") or ""),
                "file_size": row.get("file_size"),
                "doc_type": str(row.get("doc_type") or row.get("content_type") or ""),
                **dict(list_by_name.get(file_name) or {}),
            }
            result.append(enriched)
            seen.add(file_name)
        for item in list_files:
            file_name = str(item.get("file_name") or "")
            if file_name not in seen:
                result.append(dict(item))
        return result

    def _remember_document_tree_folder(self, path: str, opened: bool) -> None:
        current = {str(item) for item in (self.state.get("document_tree_open") or []) if str(item)}
        if opened:
            current.add(path)
        else:
            current.discard(path)
        self.state["document_tree_open"] = sorted(current)

    def _filter_documents_from_map(self, value: str) -> None:
        query = str(value or "").strip()
        memory = self.state.get("dataset_memory") if isinstance(self.state.get("dataset_memory"), dict) else {}
        project_pdf = self.state.get("pdf_extract") if isinstance(self.state.get("pdf_extract"), dict) else {}
        if not project_pdf and isinstance(memory, dict):
            project_pdf = memory.get("project_pdf_extract") if isinstance(memory.get("project_pdf_extract"), dict) else {}
        matches = (
            [
                str(item.get("file_name") or "")
                for item in self._composition_files(memory, project_pdf)
                if str(item.get("discipline") or "").strip() == query
            ]
            if query
            else []
        )
        self.state["document_filter"] = ""
        self.state["document_map_files"] = matches
        self.state["document_map_label"] = query
        self.view._render_documents()

    def _set_document_text_filter(self, value: str) -> None:
        self.state["document_filter"] = str(value or "")
        self.state["document_map_files"] = []
        self.state["document_map_label"] = ""
        self.view._render_documents()

    def _set_document_file_filter(self, key: str, value: str) -> None:
        self.state[key] = str(value or "")
        self.state["document_map_files"] = []
        self.state["document_map_label"] = ""
        self.view._render_documents()

    async def _inspect_composition_file(self, doc_id: str, file_name: str) -> None:
        if not doc_id:
            return
        self._content_revision += 1
        request = (self._selection_revision, self._content_revision)
        self.state["hits"] = []
        self.state["selected_doc_id"] = doc_id
        self.state["selected_doc_name"] = file_name
        self.state["view_mode"] = "map"
        self.state["map_target"] = "file"
        self.state["view_title"] = _dataset_title(self._selected_dataset_row())
        self.state["view_note"] = "Извлечённое содержимое файла и оригинал."
        self.state["composition_file_loading"] = True
        self.state["composition_file"] = {"doc_id": doc_id, "file_name": file_name}
        self.view._render_documents()
        self.view._render_view()
        chunks_request = api_get(
            f"/api/documents/by-id/{quote(doc_id, safe='')}/chunks?"
            + urlencode({"limit": 12, "max_chars": 1800})
        )
        data = await chunks_request
        if request != (self._selection_revision, self._content_revision):
            return
        self.state["composition_file_loading"] = False
        if not isinstance(data, dict):
            self.view._render_status_error()
            self.view._render_view()
            return
        self.state["composition_file"] = {
            "doc_id": doc_id,
            "file_name": file_name,
            "document": dict(data.get("document") or {}),
            "chunks": list(data.get("chunks") or []),
            "total": int(data.get("total") or 0),
        }
        self.view._render_documents()
        self.view._render_view()


    def _show_dataset_data(self) -> None:
        self._content_revision += 1
        self.state["hits"] = []
        self.state["view_mode"] = "map"
        self.state["map_target"] = "dataset"
        self.state["composition_file"] = {}
        self.state["composition_file_loading"] = False
        self.state["view_title"] = _dataset_title(self._selected_dataset_row()) if self.state["selected_dataset"] else "Выберите датасет"
        self.state["view_note"] = "Паспорт, состав и извлечённые данные датасета."
        self.view._render_documents()
        self.view._render_view()

    async def _load_datasets(self, select_first: bool = True) -> None:
        params = {"limit": 400}
        if self.state["dataset_filter"].strip():
            params["q"] = self.state["dataset_filter"].strip()
        data = await api_get("/api/documents/datasets?" + urlencode(params))
        if not isinstance(data, dict):
            self.view._render_status_error()
            return
        self.state["datasets"] = list(data.get("datasets") or [])
        if select_first and self.state["datasets"] and not self.state["selected_dataset"]:
            await self._select_dataset(str(self.state["datasets"][0].get("id") or ""))
        else:
            self.view._render_datasets()
            self.view._render_documents()
            self.view._render_view()

    async def _select_dataset(self, dataset_id: str) -> None:
        self._selection_revision += 1
        self._content_revision += 1
        revision = self._selection_revision
        self.state["selected_dataset"] = dataset_id
        self.state["documents"] = []
        self.state["selected_doc_id"] = ""
        self.state["selected_doc_name"] = ""
        self.state["selected_doc_ids"] = []
        self.state["chunks"] = []
        self.state["hits"] = []
        self.state["dataset_memory"] = {}
        self.state["memory_loading"] = False
        self.state["pdf_extract"] = {}
        self.state["pdf_extract_loading"] = False
        self.state["operator_guidance"] = ""
        self.state["selected_folder"] = ""
        self.state["composition_file"] = {}
        self.state["composition_file_loading"] = False
        self.state["map_target"] = "dataset"
        self.state["document_tree_open"] = []
        self.state["document_map_files"] = []
        self.state["document_map_label"] = ""
        for key in (
            "document_folder_filter", "document_extension_filter",
            "document_status_filter", "document_role_filter",
        ):
            self.state[key] = ""
        self.state["dataset_kind"] = str(self._selected_dataset_row().get("dataset_kind") or "")
        self.state["view_mode"] = self.initial_mode
        self.state["view_title"] = _dataset_title(self._selected_dataset_row()) if dataset_id else "Выберите датасет"
        self.state["view_note"] = self.initial_note
        service_upload = self.refs.get("service_upload")
        if service_upload is not None:
            service_upload.set_visibility(self._is_system_dataset() and self.can_manage)
        await self._load_documents()
        if revision != self._selection_revision:
            return
        await asyncio.gather(
            self._load_memory(),
            self._load_pdf_extract_summary(),
            self._load_rag_readiness(dataset_id),
        )

    async def _load_rag_readiness(self, dataset_id: str = "", *, force: bool = False) -> None:
        revision = self._selection_revision
        self.state["rag_readiness_loading"] = True
        params = {}
        if dataset_id:
            params["dataset_id"] = dataset_id
        if force:
            params["force"] = "true"
        suffix = "?" + urlencode(params) if params else ""
        data = await api_get("/api/rag/readiness" + suffix)
        if revision != self._selection_revision:
            return
        self.state["rag_readiness_loading"] = False
        self.state["rag_readiness"] = data if isinstance(data, dict) else {}
        self.view._render_readiness_summary()
        self.view._render_datasets()
        self.view._render_view()




    async def _load_memory(self) -> None:
        revision = self._selection_revision
        dataset_id = self.state["selected_dataset"]
        if not dataset_id:
            self.state["dataset_memory"] = {}
            self.view._render_view()
            return
        self.state["memory_loading"] = True
        self.view._render_view()
        data = await api_get(f"/api/notebooks/{quote(dataset_id, safe='')}/memory")
        if revision != self._selection_revision:
            return
        self.state["memory_loading"] = False
        if not isinstance(data, dict):
            self.view._render_status_error()
            return
        self.state["dataset_memory"] = data
        self.state["operator_guidance"] = str(data.get("operator_guidance") or "")
        self.state["dataset_kind"] = str(data.get("dataset_kind") or self.state.get("dataset_kind") or "")
        self.view._render_view()

    async def _load_pdf_extract_summary(self) -> None:
        revision = self._selection_revision
        dataset_id = self.state["selected_dataset"]
        if not dataset_id:
            ui.notify("Сначала выберите датасет", type="warning")
            return
        self.state["pdf_extract_loading"] = True
        self.view._render_view()
        data = await api_get(f"/api/rag/datasets/{quote(dataset_id, safe='')}/pdf-extract/summary")
        if revision != self._selection_revision:
            return
        self.state["pdf_extract_loading"] = False
        if not isinstance(data, dict):
            self.view._render_status_error()
            self.view._render_view()
            return
        self.state["pdf_extract"] = data
        self.view._render_view()

    async def _load_documents(self) -> None:
        self._list_revision += 1
        request = (self._selection_revision, self._list_revision)
        dataset_id = self.state["selected_dataset"]
        if not dataset_id:
            self.state["documents"] = []
            self.view._render_all()
            return
        params = {"limit": 1000}
        if self.state["document_filter"].strip():
            params["q"] = self.state["document_filter"].strip()
        path = f"/api/documents/datasets/{quote(dataset_id, safe='')}/documents?{urlencode(params)}"
        data = await api_get(path)
        if request != (self._selection_revision, self._list_revision):
            return
        if not isinstance(data, dict):
            self.view._render_status_error()
            return
        self.state["documents"] = sorted(list(data.get("documents") or []), key=_file_sort_key)
        self.view._render_all()

    async def _open_native_document(self, doc_id: str) -> None:
        if not doc_id:
            ui.notify("Документ не найден в индексе", type="warning")
            return
        data = await api_post(f"/api/documents/by-id/{quote(doc_id, safe='')}/open-native")
        if not isinstance(data, dict):
            self.view._render_status_error()
            return
        if data.get("status") == "opened":
            ui.notify("Файл открыт в системном приложении", type="positive")
        else:
            ui.notify(str(data.get("error") or data.get("status") or "Не удалось открыть файл"), type="warning")

    async def _open_native_file_name(self, file_name: str, doc_id: str = "") -> None:
        doc_id = str(doc_id or "").strip()
        if not doc_id:
            doc = self._document_by_file_name(file_name)
            doc_id = str(doc.get("id") or "").strip()
        await self._open_native_document(doc_id)

    async def _search(self, scope: str) -> None:
        query = self.state["query"].strip()
        if not query:
            ui.notify("Введите поисковую фразу", type="warning")
            return
        self._content_revision += 1
        request = (self._selection_revision, self._content_revision)
        params: dict[str, object] = {"q": query, "limit": 80, "max_chars": 2200}
        if scope == "dataset" and self.state["selected_dataset"]:
            params["dataset_id"] = [self.state["selected_dataset"]]
        if scope == "document" and self.state["selected_doc_id"]:
            params["doc_id"] = self.state["selected_doc_id"]
        query_string = urlencode(params, doseq=True)
        data = await api_get("/api/documents/search?" + query_string)
        if request != (self._selection_revision, self._content_revision):
            return
        if not isinstance(data, dict):
            self.view._render_status_error()
            return
        self.state["hits"] = list(data.get("hits") or [])
        self.state["chunks"] = []
        self.state["view_mode"] = "fragments"
        scope_label = {
            "document": "в документе",
            "dataset": "в датасете",
            "all": "во всём индексе",
        }.get(scope, "в индексе")
        self.state["view_title"] = f"Поиск {scope_label}: {query}"
        self.state["view_note"] = f"Найдено {data.get('count', len(self.state['hits']))}. Источник: lexical SQLite/FTS."
        self.view._render_view()

    def _toggle_document_selection(self, doc_id: str) -> None:
        selected = [str(value) for value in (self.state.get("selected_doc_ids") or [])]
        if doc_id in selected:
            selected.remove(doc_id)
        else:
            selected.append(doc_id)
        self.state["selected_doc_ids"] = selected
        self.view._render_documents()
        self.view._render_view()

    def _activate_document_row(self, doc_id: str, file_name: str) -> None:
        _schedule(self._inspect_composition_file(doc_id, file_name))

    def _ask_about_selected_documents(self) -> None:
        selected = {str(value) for value in (self.state.get("selected_doc_ids") or [])}
        files = [
            str(row.get("file_name") or "")
            for row in (self.state.get("documents") or [])
            if str(row.get("id") or "") in selected and str(row.get("file_name") or "")
        ]
        dataset_id = str(self.state.get("selected_dataset") or "")
        if not dataset_id or not files:
            ui.notify("Выберите документы", type="warning")
            return
        params = {
            "scope": f"ds:{dataset_id}",
            "target_files": json.dumps(files, ensure_ascii=False, separators=(",", ":")),
            "tab": "chat",
        }
        path = str(getattr(context.client.request, "url", "") or "")
        target_path = "/les/classic" if "/les/classic" in path else "/classic"
        ui.navigate.to(f"{target_path}?{urlencode(params)}")

    async def _upload_service_file(self, event) -> None:
        dataset_id = str(self.state.get("selected_dataset") or "")
        if not dataset_id or not self._is_system_dataset():
            ui.notify("Сначала выберите служебный датасет", type="warning")
            return
        try:
            upload = getattr(event, "file", None)
            if upload is not None and hasattr(upload, "read"):
                content = await upload.read()
                file_name = getattr(upload, "name", "") or getattr(event, "name", "") or "document.bin"
            else:
                raw = getattr(event, "content", None)
                if raw is None or not hasattr(raw, "read"):
                    raise AttributeError("не удалось прочитать выбранный файл")
                value = raw.read()
                content = await value if inspect.isawaitable(value) else value
                file_name = getattr(event, "name", "") or "document.bin"
            if isinstance(content, str):
                content = content.encode("utf-8")
            if not content:
                raise ValueError("файл пуст")
        except Exception as error:
            ui.notify(f"Не удалось прочитать файл: {error}", type="negative")
            return
        ui.notify(f"Добавляю «{file_name}»", type="info")
        result = await api_post_file(f"/api/rag/upload/{quote(dataset_id, safe='')}", content, file_name)
        if not isinstance(result, dict):
            ui.notify(last_api_error_text("Не удалось добавить файл"), type="negative")
            return
        ui.notify("Файл добавлен. Индексация выполняется в фоне.", type="positive")
        await self._load_documents()
        await self._load_datasets(select_first=False)

    def _set_dataset_group_filter(self, group: str) -> None:
        self.state["dataset_group_filter"] = group
        for value, button in (self.refs.get("dataset_group_buttons") or {}).items():
            button.classes(remove="sov-dataset-group-btn--active")
            if value == group:
                button.classes(add="sov-dataset-group-btn--active")
        self.view._render_datasets()

    async def _load_surface(self) -> None:
        await self._load_datasets()
        if self.initial_dataset and any(
            str(row.get("id") or "") == self.initial_dataset for row in self.state["datasets"]
        ):
            await self._select_dataset(self.initial_dataset)
