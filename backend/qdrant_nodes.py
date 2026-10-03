"""Typed source nodes and structural context."""
from __future__ import annotations
from backend import qdrant_support as support


class QdrantNodes:
    def _apply_context_metadata(self, file_nodes: list[dict], dataset_id: str, file_key: str) -> None:
        support._apply_context_metadata_to_nodes(file_nodes, dataset_id, file_key)

    @staticmethod
    def _finalize_embedding_nodes(
        file_nodes: list[dict],
        *,
        chunking: dict[str, support.Any],
    ) -> list[dict]:
        """Sanitize and enforce the final embedding budget for all node types."""
        len_fn = chunking.get("len_fn") or len
        budget = max(1, int(chunking.get("chunk_size") or 1))
        unit = str(chunking.get("unit") or "chars")
        finalized: list[dict] = []
        for node_index, node in enumerate(file_nodes):
            clean, quality = support._sanitize_embedding_text(str(node.get("text") or ""))
            if len(clean) < support.FINAL_MIN_CHUNK:
                continue
            parts = support._split_to_embedding_budget(clean, budget=budget, len_fn=len_fn)
            parent_id = str(node.get("doc_id") or f"node-{node_index}")
            for child_index, part in enumerate(parts):
                if len(part) < support.FINAL_MIN_CHUNK:
                    continue
                measured = int(len_fn(part))
                if measured > budget:
                    raise RuntimeError(
                        f"embedding budget invariant failed: {measured}>{budget} {unit}"
                    )
                payload = dict(node.get("payload") or {})
                payload.update(
                    {
                        "embedding_chunk_unit": unit,
                        "embedding_chunk_length": measured,
                        "embedding_chunk_budget": budget,
                        "embedding_budget_enforced": True,
                        "content_sanitized": quality["sanitized"],
                        "content_quality": quality,
                        "parent_node_id": payload.get("parent_node_id") or parent_id,
                        "child_ord": child_index,
                    }
                )
                suffix = support.hashlib.sha1(
                    f"{parent_id}\n{child_index}\n{part}".encode("utf-8", errors="ignore")
                ).hexdigest()[:16]
                finalized.append(
                    {
                        **node,
                        "text": part,
                        "doc_id": f"{parent_id}:budget:{suffix}",
                        "payload": payload,
                    }
                )
        return finalized

    def _sync_markdown_nodes(
        self,
        file_path: support.Path,
        file_key: str,
        dataset_id: str,
        md_parser: support.MarkdownNodeParser,
        splitter: support.SentenceSplitter,
        route: support.DocumentRoute | None = None,
        timings: dict[str, float] | None = None,
    ) -> list[dict]:
        import time as _t
        phase_start = _t.time()
        md_content = support.convert_to_markdown_for_indexing(file_path, route=route)
        if timings is not None:
            timings["convert_sec"] = timings.get("convert_sec", 0.0) + (_t.time() - phase_start)
        if not md_content:
            return []

        phase_start = _t.time()
        if support._pdf_page_nodes_enabled(file_path, route):
            file_nodes = self._sync_pdf_page_text_nodes(
                file_key,
                dataset_id,
                md_content,
                route,
                file_path=file_path,
            )
            if timings is not None:
                timings["chunk_sec"] = timings.get("chunk_sec", 0.0) + (_t.time() - phase_start)
            if file_nodes:
                return file_nodes

        doc = support.Document(
            text=md_content,
            metadata={"file_name": file_key, "dataset_id": dataset_id},
        )
        nodes = md_parser.get_nodes_from_documents([doc])

        file_nodes = []
        payload_type = "spreadsheet_projection" if file_path.suffix.lower() in {".xlsx", ".xlsm", ".xls", ".csv"} else "markdown"
        # A short standalone note is still evidence. The ordinary chunk floor
        # removes fragments of long documents, not entire readable short files.
        minimum = support.FINAL_MIN_CHUNK if len(md_content.strip()) < support.MIN_CHUNK else support.MIN_CHUNK
        for node in nodes:
            node.metadata.update(doc.metadata)
            if len(node.text) > 2000:
                split_nodes = splitter.get_nodes_from_documents([node])
                file_nodes.extend(
                    {
                        "text": split_node.text,
                        "doc_id": split_node.node_id,
                        "payload": self._route_payload(route, {"type": payload_type}),
                    }
                    for split_node in split_nodes
                    if len(split_node.text) >= support.MIN_CHUNK
                )
            elif len(node.text.strip()) >= minimum:
                file_nodes.append({
                    "text": node.text,
                    "doc_id": node.node_id,
                    "payload": self._route_payload(route, {"type": payload_type}),
                })
        if timings is not None:
            timings["chunk_sec"] = timings.get("chunk_sec", 0.0) + (_t.time() - phase_start)
        return file_nodes

    def _sync_pdf_page_text_nodes(
        self,
        file_key: str,
        dataset_id: str,
        md_content: str,
        route: support.DocumentRoute | None = None,
        *,
        file_path: support.Path | None = None,
    ) -> list[dict]:
        page_blocks = self._split_pdf_page_markdown(md_content)
        if not page_blocks:
            return []
        page_passports: dict[int, dict[str, support.Any]] = {}
        if file_path is not None and support._pdf_page_passport_enabled(file_path):
            try:
                from proxy.services.pdf_contour_service import rag_page_metadata

                page_passports = rag_page_metadata(
                    file_path,
                    [page_no for page_no, _text in page_blocks],
                    file_name=file_key,
                )
            except Exception as error:  # noqa: BLE001 - passport enrichment must not lose searchable text
                support.logger.warning("[PDF-CONTOUR] page passport failed for %s: %s", file_key, error)
        max_chars = support._pdf_page_node_max_chars()
        overlap = min(support._pdf_page_node_overlap_chars(), max_chars // 3)
        nodes: list[dict] = []
        for page_no, page_text in page_blocks:
            text = page_text.strip()
            if not text or text == "[no text extracted]":
                continue
            chunks = self._split_pdf_page_text(text, max_chars=max_chars, overlap=overlap)
            part_count = len(chunks)
            passport = page_passports.get(page_no) or {}
            quality = passport.get("recognition_quality") if isinstance(passport.get("recognition_quality"), dict) else {}
            signals = passport.get("signals") if isinstance(passport.get("signals"), dict) else {}
            stamp = passport.get("stamp") if isinstance(passport.get("stamp"), dict) else {}
            fragment_bboxes = [
                {
                    "fragment_id": item.get("fragment_id"),
                    "bbox": item.get("bbox"),
                    "source_ref": item.get("source_ref"),
                }
                for item in (passport.get("evidence_fragments") or [])
                if isinstance(item, dict)
            ][:5]
            route_uses_ocr = str(getattr(route, "pipeline", "") or "") == "markdown_needs_ocr"
            source_layer = "pdf_ocr_text" if route_uses_ocr or passport.get("requires_ocr") else "pdf_text_layer"
            for part_no, chunk_text in enumerate(chunks, start=1):
                # A short PDF page can still be decisive evidence: a cover mark,
                # sheet number, room code or equipment designation.  The generic
                # markdown noise threshold must not discard an entire non-empty
                # page after the page router has already identified it.
                if not chunk_text.strip():
                    continue
                title = f"## Page {page_no}"
                if part_count > 1:
                    title += f" part {part_no}/{part_count}"
                text_for_index = f"{title}\n\n{chunk_text.strip()}"
                payload = {
                    "type": "pdf_page_text",
                    "source_layer": source_layer,
                    "page": page_no,
                    "page_part": part_no,
                    "page_parts": part_count,
                    "source_ref": str(passport.get("source_ref") or f"{file_key}#page={page_no}"),
                }
                if passport:
                    payload.update({
                        "pdf_page_passport_schema": passport.get("schema"),
                        "pdf_page_type": passport.get("page_type"),
                        "pdf_page_type_label": passport.get("page_type_label"),
                        "pdf_routing_confidence": passport.get("routing_confidence"),
                        "pdf_requires_ocr": bool(passport.get("requires_ocr")),
                        "pdf_recognition_quality": quality,
                        "pdf_page_signals": signals,
                        "pdf_stamp_status": stamp.get("status"),
                        "pdf_sheet_number": stamp.get("sheet_number") or "",
                        "pdf_fragment_bboxes": fragment_bboxes,
                    })
                nodes.append({
                    "text": text_for_index,
                    "doc_id": str(support.uuid.uuid5(
                        support.uuid.NAMESPACE_URL,
                        f"{dataset_id}:{file_key}:pdf-page:{page_no}:{part_no}",
                    )),
                    "payload": self._route_payload(route, payload),
                })
        return nodes

    @staticmethod
    def _split_pdf_page_markdown(md_content: str) -> list[tuple[int, str]]:
        matches = list(support.re.finditer(r"(?m)^## (?:Page|Стр\.)\s+(\d+)\s*$", md_content or ""))
        pages: list[tuple[int, str]] = []
        for index, match in enumerate(matches):
            start = match.end()
            end = matches[index + 1].start() if index + 1 < len(matches) else len(md_content)
            try:
                page_no = int(match.group(1))
            except ValueError:
                continue
            pages.append((page_no, md_content[start:end].strip()))
        return pages

    @staticmethod
    def _split_pdf_page_text(text: str, *, max_chars: int, overlap: int) -> list[str]:
        value = (text or "").strip()
        if len(value) <= max_chars:
            return [value] if value else []
        chunks: list[str] = []
        start = 0
        while start < len(value):
            end = min(len(value), start + max_chars)
            if end < len(value):
                boundary = max(value.rfind("\n", start, end), value.rfind(". ", start, end))
                if boundary > start + max_chars // 2:
                    end = boundary + (1 if value[boundary] == "\n" else 2)
            chunk = value[start:end].strip()
            if chunk:
                chunks.append(chunk)
            if end >= len(value):
                break
            start = max(end - overlap, start + 1)
        return chunks

    def _sync_mail_nodes(
        self,
        file_path: support.Path,
        data_dir: support.Path,
        file_key: str,
        dataset_id: str,
        splitter: support.SentenceSplitter,
        route: support.DocumentRoute | None = None,
        timings: dict[str, float] | None = None,
    ) -> list[dict]:
        import time as _t

        phase_start = _t.time()
        profile = support.build_mail_vector_profile(file_path, source_dir=data_dir)
        if timings is not None:
            timings["convert_sec"] = timings.get("convert_sec", 0.0) + (_t.time() - phase_start)

        phase_start = _t.time()
        nodes: list[dict] = []
        message_payload = profile.payload()
        registry_payload: dict[str, support.Any] = {}
        registry = None
        registered = None
        try:
            from proxy.services.mail_registry_service import get_mail_registry

            registry = get_mail_registry()
            registered = registry.find_message_by_relative_path(file_key)
            if registered:
                account = registry.get_account(registered["account_id"], include_secret_state=False)
                if str(account.get("dataset_id") or "") == dataset_id:
                    registry_payload = {
                        "mail_account_id": registered["account_id"],
                        "mail_registry_message_id": registered["id"],
                        "mail_dataset_id": account["dataset_id"],
                        "mail_dataset_name": account["dataset_name"],
                        "mail_source_kind": registered["source_kind"],
                        "mail_source_locator": {
                            "outlook_store_id": registered["outlook_store_id"],
                            "outlook_entry_id": registered["outlook_entry_id"],
                            "native_id": registered["native_id"],
                        },
                        "mail_content_sha256": registered["content_sha256"],
                        "mail_folders": [
                            item["folder_path"]
                            for item in registered["locations"]
                            if item.get("is_current")
                        ],
                    }
        except Exception:
            # Legacy MAIL_Index and standalone parser tests have no registry.
            registry_payload = {}
        message_payload.update(registry_payload)
        message_payload.update({
            "type": "mail_message",
            "mail_node_kind": "message",
        })
        nodes.extend(
            self._split_profile_text_nodes(
                profile.message_embedding_text(include_attachment_text=False),
                dataset_id,
                file_key,
                splitter,
                self._route_payload(route, message_payload),
                "message",
            )
        )

        for attachment in profile.attachments:
            attachment_provenance: dict[str, support.Any] = {}
            if registry is not None and registered is not None and registry_payload:
                attachment_provenance = registry.register_attachment_provenance(
                    account_id=registered["account_id"],
                    message_id=registered["id"],
                    attachment_id=attachment.attachment_id,
                    attachment_sha256=attachment.sha256,
                )
                if not attachment_provenance.get("canonical", True):
                    # The per-message node above retains this message's attachment
                    # checksum/provenance; identical attachment text is embedded once.
                    continue
            attachment_payload = profile.payload()
            attachment_payload.update(registry_payload)
            attachment_payload.update({
                "type": "mail_attachment",
                "mail_node_kind": "attachment",
                "mail_attachment_id": attachment.attachment_id,
                "mail_attachment_filename": attachment.filename,
                "mail_attachment_content_type": attachment.content_type,
                "mail_attachment_kind": attachment.kind,
                "mail_attachment_sha256": attachment.sha256,
                "mail_attachment_extraction": attachment.extraction,
                "mail_attachment_needs_ocr": attachment.needs_ocr,
                "mail_attachment_needs_vlm": attachment.needs_vlm,
                "mail_attachment_has_text": attachment.has_text,
                "mail_attachment_error": attachment.error,
                "mail_attachment_canonical_message_id": attachment_provenance.get(
                    "canonical_message_id", registered["id"] if registered else ""
                ),
                "mail_attachment_provenance_count": attachment_provenance.get(
                    "provenance_count", 1
                ),
            })
            nodes.extend(
                self._split_profile_text_nodes(
                    attachment.embedding_text(profile),
                    dataset_id,
                    file_key,
                    splitter,
                    self._route_payload(route, attachment_payload),
                    f"attachment:{attachment.attachment_id}",
                )
            )

        if timings is not None:
            timings["chunk_sec"] = timings.get("chunk_sec", 0.0) + (_t.time() - phase_start)
        return nodes

    def _split_profile_text_nodes(
        self,
        text: str,
        dataset_id: str,
        file_key: str,
        splitter: support.SentenceSplitter,
        payload: dict[str, support.Any],
        node_key: str,
    ) -> list[dict]:
        value = str(text or "").strip()
        if len(value) < support.MIN_CHUNK:
            return []
        if len(value) <= 2000:
            return [{
                "text": value,
                "doc_id": support.deterministic_mail_node_id(dataset_id, file_key, node_key),
                "payload": dict(payload),
            }]

        doc = support.Document(text=value, metadata={"file_name": file_key, "dataset_id": dataset_id})
        split_nodes = splitter.get_nodes_from_documents([doc])
        return [
            {
                "text": split_node.text,
                "doc_id": support.deterministic_mail_node_id(dataset_id, file_key, f"{node_key}:{idx}"),
                "payload": dict(payload),
            }
            for idx, split_node in enumerate(split_nodes)
            if len(split_node.text) >= support.MIN_CHUNK
        ]

    def _sync_table_nodes(
        self,
        file_path: support.Path,
        data_dir: support.Path,
        file_key: str,
        dataset_id: str,
        route: support.DocumentRoute | None = None,
        timings: dict[str, float] | None = None,
    ) -> list[dict]:
        import time as _t
        # Производный Parquet кладём в storage по file_key (а не относительно file_path):
        # внешний in-place источник лежит вне data_dir, но дериватив остаётся в storage.
        parquet_dir = data_dir / "_parquet" / support.Path(file_key).parent
        normalizer = support.TableNormalizer(parquet_dir=str(parquet_dir), use_llm=False)
        phase_start = _t.time()
        doc_type_override = "TABLE" if route and not route.domain.startswith("TABLE_") else None
        result = support.asyncio.run(
            normalizer.process(str(file_path), dataset_id=dataset_id, doc_type_override=doc_type_override)
        )
        if timings is not None:
            timings["convert_sec"] = timings.get("convert_sec", 0.0) + (_t.time() - phase_start)
        parquet_path = result.get("parquet_path") or ""
        parquet_rel = ""
        if parquet_path:
            try:
                parquet_rel = support.Path(parquet_path).relative_to(data_dir).as_posix()
            except ValueError:
                parquet_rel = parquet_path

        phase_start = _t.time()
        chunks = list(result.get("chunks") or [])
        if len(chunks) > support.TABLE_ROW_INDEX_MAX_CHUNKS:
            return self._table_navigation_projection_nodes(
                file_path=file_path,
                file_key=file_key,
                dataset_id=dataset_id,
                route=route,
                parquet_rel=parquet_rel,
                result=result,
                chunks=chunks,
                timings=timings,
                phase_start=phase_start,
            )

        nodes = []
        for i, chunk in enumerate(chunks):
            text = str(chunk.get("text") or "")
            if len(text) < support.MIN_CHUNK:
                continue
            payload = dict(chunk.get("metadata") or {})
            payload.update({
                "type": "table_row",
                "parquet_path": parquet_rel,
                "table_row": i,
                "table_kind": self._table_kind(route),
            })
            nodes.append({
                "text": text,
                "doc_id": str(support.uuid.uuid5(support.uuid.NAMESPACE_URL, f"{dataset_id}:{file_path}:{i}")),
                "payload": self._route_payload(route, payload),
            })
        if not nodes and result.get("needs_ocr"):
            scanned_pages = result.get("scanned_pages") or []
            text = (
                f"PDF {file_path.name} содержит страницы без текстового слоя; "
                f"нужна OCR/VLM обработка. Страницы: {', '.join(map(str, scanned_pages)) or '?'}"
            )
            payload = {
                "type": "pdf_needs_ocr",
                "needs_ocr": True,
                "scanned_pages": scanned_pages,
                "parquet_path": "",
                "table_kind": self._table_kind(route),
            }
            nodes.append({
                "text": text,
                "doc_id": str(support.uuid.uuid5(support.uuid.NAMESPACE_URL, f"{dataset_id}:{file_path}:needs_ocr")),
                "payload": self._route_payload(route, payload),
            })
        if timings is not None:
            timings["chunk_sec"] = timings.get("chunk_sec", 0.0) + (_t.time() - phase_start)
        return nodes

    def _table_navigation_projection_nodes(
        self,
        *,
        file_path: support.Path,
        file_key: str,
        dataset_id: str,
        route: support.DocumentRoute | None,
        parquet_rel: str,
        result: dict[str, support.Any],
        chunks: list[dict[str, support.Any]],
        timings: dict[str, float] | None,
        phase_start: float,
    ) -> list[dict]:
        import time as _t

        rows = int(result.get("rows") or len(chunks))
        names: list[str] = []
        codes: list[str] = []
        units: list[str] = []
        sections: list[str] = []
        for chunk in chunks:
            meta = dict(chunk.get("metadata") or {})
            for target, key in ((names, "name"), (codes, "code"), (units, "unit"), (sections, "section")):
                value = str(meta.get(key) or "").strip()
                if value and value not in target:
                    target.append(value[:160])
                if len(target) >= 20:
                    break

        text = "\n".join(
            part
            for part in [
                f"Табличный файл: {file_key}",
                "Тип: table_navigation_projection",
                f"Строк нормализовано: {rows}",
                f"Листов/таблиц: {result.get('sheets') or ''}",
                f"Parquet: {parquet_rel or '[не создан]'}",
                "Назначение: навигация по таблице и выбор источника; точные строки, фильтры, суммы и группировки читать из parquet/source table reader.",
                "Разделы: " + "; ".join(sections[:20]) if sections else "",
                "Коды/обозначения: " + "; ".join(codes[:20]) if codes else "",
                "Наименования: " + "; ".join(names[:20]) if names else "",
                "Единицы: " + "; ".join(units[:20]) if units else "",
            ]
            if part
        )
        payload = {
            "type": "table_navigation_projection",
            "parquet_path": parquet_rel,
            "table_kind": self._table_kind(route),
            "table_rows": rows,
            "table_sheets": result.get("sheets") or 0,
            "source_file": file_path.name,
        }
        if timings is not None:
            timings["chunk_sec"] = timings.get("chunk_sec", 0.0) + (_t.time() - phase_start)
        return [{
            "text": text,
            "doc_id": str(support.uuid.uuid5(support.uuid.NAMESPACE_URL, f"{dataset_id}:{file_path}:table_projection")),
            "payload": self._route_payload(route, payload),
        }]

    @staticmethod
    def _docx_table_extraction_enabled(file_path: support.Path, route: support.DocumentRoute | None) -> bool:
        if file_path.suffix.lower() != ".docx":
            return False
        if support.os.getenv("DOCX_TABLE_EXTRACTION_ENABLED", "false").lower() not in ("1", "true", "yes"):
            return False
        if route is None:
            return True
        return route.domain.startswith("NTD_") or route.domain in {"GKRF", "BOOKS"}

    @staticmethod
    def _table_kind(route: support.DocumentRoute | None) -> str:
        if route is None:
            return "table"
        if route.domain.startswith("TABLE_"):
            return "cost"
        if route.domain.startswith("NTD_") or route.domain in {"GKRF", "BOOKS"}:
            return "normative"
        return "table"

    def _route_payload(self, route: support.DocumentRoute | None, payload: dict) -> dict:
        if route is None:
            return payload
        merged = dict(payload)
        merged.update(route.metadata)
        return merged

