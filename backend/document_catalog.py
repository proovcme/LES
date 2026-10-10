"""SQLite document catalog, migrations, source fingerprints and parse status.

Owns metadata only; vector storage and model requests belong to separate adapters.
"""
import os
import sqlite3
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Dict, List, Optional
from backend.converter import normalize_pdf_text
from backend.document_router import DocumentRoute
from backend.interface import DatasetInfo
from backend.rag_config import rag_collection_name, rag_meta_db_path

class MetaDB:
    def __init__(self, db_path: str | None = None):
        db_path = db_path or rag_meta_db_path()
        self.db_path = db_path
        Path(db_path).parent.mkdir(parents=True, exist_ok=True)
        self._init_db()

    @contextmanager
    def _get_conn(self):
        conn = sqlite3.connect(self.db_path, check_same_thread=False)
        try:
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA synchronous=NORMAL")
            with conn:
                yield conn
        finally:
            conn.close()

    def _init_db(self):
        with self._get_conn() as conn:
            conn.execute("""
                CREATE TABLE IF NOT EXISTS datasets (
                    id          TEXT PRIMARY KEY,
                    name        TEXT,
                    status      TEXT,
                    chunk_count INTEGER DEFAULT 0
                )
            """)
            conn.execute("""
                CREATE TABLE IF NOT EXISTS documents (
                    id          TEXT PRIMARY KEY,
                    dataset_id  TEXT,
                    file_name   TEXT,
                    status      TEXT,
                    file_hash   TEXT,
                    file_mtime  REAL,
                    file_size   INTEGER,
                    chunk_count INTEGER DEFAULT 0
                )
            """)
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_docs_dataset ON documents(dataset_id)"
            )
            conn.execute("""
                CREATE TABLE IF NOT EXISTS structured_rules (
                    id          TEXT PRIMARY KEY,
                    document_id TEXT NOT NULL,
                    file_key    TEXT NOT NULL,
                    chunk_id    TEXT NOT NULL,
                    subject     TEXT NOT NULL,
                    parameter   TEXT NOT NULL,
                    operator    TEXT NOT NULL,
                    value       REAL NOT NULL,
                    unit        TEXT NOT NULL,
                    condition   TEXT,
                    char_start  INTEGER NOT NULL,
                    char_end    INTEGER NOT NULL,
                    created_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                )
            """)
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_rules_doc ON structured_rules(document_id)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_rules_file ON structured_rules(file_key)"
            )
            # Inspect schema before migration. Disk/permission/SQL errors must propagate.
            document_columns = {row[1] for row in conn.execute("PRAGMA table_info(documents)")}
            for col, typedef in [
                ("file_hash",   "TEXT"),
                ("file_mtime",  "REAL"),
                ("file_size",   "INTEGER"),
                ("chunk_count", "INTEGER DEFAULT 0"),
                ("domain",      "TEXT DEFAULT ''"),
                ("route_dataset", "TEXT DEFAULT ''"),
                ("doc_type",    "TEXT DEFAULT ''"),
                ("content_type", "TEXT DEFAULT ''"),
                ("complexity",   "TEXT DEFAULT ''"),
                ("pipeline",     "TEXT DEFAULT ''"),
                ("last_error",   "TEXT DEFAULT ''"),
                ("stage",        "TEXT DEFAULT ''"),  # W1.4: текущая стадия конвейера (CONVERT/EMBED/UPSERT)
                ("source_path",  "TEXT DEFAULT ''"),  # внешний in-place источник (абсолютный путь, без копии в storage)
                ("parse_attempts", "INTEGER DEFAULT 0"),
                ("last_attempt_at", "REAL DEFAULT 0"),
                ("retryable", "INTEGER DEFAULT 0"),
                ("retry_after", "REAL DEFAULT 0"),
                ("error_code", "TEXT DEFAULT ''"),
            ]:
                if col not in document_columns:
                    conn.execute(f"ALTER TABLE documents ADD COLUMN {col} {typedef}")
            dataset_columns = {row[1] for row in conn.execute("PRAGMA table_info(datasets)")}
            for column, definition in (
                ('chunk_count', 'INTEGER DEFAULT 0'),
                ('sensitivity', "TEXT DEFAULT 'P0'"),
                ('group_name', "TEXT DEFAULT ''"),
                ('dataset_scope', "TEXT DEFAULT 'user'"),
                ('module_id', "TEXT DEFAULT ''"),
            ):
                if column not in dataset_columns:
                    conn.execute(f"ALTER TABLE datasets ADD COLUMN {column} {definition}")
            # Normalize the stable code only. Requeueing is performed later by
            # the explicit bounded repair pass, never as an unbounded migration.
            conn.execute(
                "UPDATE documents SET error_code='SPARSE_VECTOR_PREVALIDATION_MISSING' "
                "WHERE status='ERROR' AND last_error='missing prevalidated sparse vector'"
            )
            conn.execute("""
                CREATE TABLE IF NOT EXISTS indexing_repair_state (
                    singleton       INTEGER PRIMARY KEY CHECK(singleton=1),
                    ran_at          REAL DEFAULT 0,
                    repaired_files  INTEGER DEFAULT 0,
                    eligible_files  INTEGER DEFAULT 0,
                    max_files       INTEGER DEFAULT 0,
                    status          TEXT DEFAULT 'never'
                )
            """)
            conn.execute(
                "INSERT OR IGNORE INTO indexing_repair_state(singleton) VALUES (1)"
            )

    def ensure_system_datasets(self) -> list[str]:
        """Provision module-owned datasets only from the real runtime bootstrap.

        Constructing an isolated MetaDB must remain free of product-data side
        effects; tests, tools and import probes legitimately use temporary DBs.
        """
        from proxy.services.system_dataset_service import ensure_system_datasets

        with self._get_conn() as conn:
            return ensure_system_datasets(conn)

    def create_dataset(self, name: str) -> str:
        from proxy.services.system_dataset_service import dataset_identity, system_dataset_spec

        spec = system_dataset_spec(name)
        dataset_scope, module_id = dataset_identity(name)
        with self._get_conn() as conn:
            if spec:
                existing = conn.execute("SELECT id FROM datasets WHERE name=? LIMIT 1", (name,)).fetchone()
                if existing:
                    return str(existing[0])
            ds_id = str(uuid.uuid4())
            conn.execute(
                "INSERT INTO datasets (id, name, status, dataset_scope, module_id) "
                "VALUES (?, ?, 'IDLE', ?, ?)",
                (ds_id, name, dataset_scope, module_id),
            )
        return ds_id

    def update_dataset_status(self, dataset_id: str, status: str):
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE datasets SET status=? WHERE id=?", (status, dataset_id)
            )

    def recover_interrupted_parsing(self) -> int:
        with self._get_conn() as conn:
            dataset_cur = conn.execute("UPDATE datasets SET status='IDLE' WHERE status='PARSING'")
            max_attempts = max(1, int(os.getenv("RAG_PARSE_MAX_ATTEMPTS", "4")))
            now = time.time()
            cur = conn.execute(
                "UPDATE documents SET status='PENDING', stage='', retryable=0 "
                "WHERE (status IN ('QUEUED','PARSING','RUNNING')) "
                "OR (status='PENDING' AND COALESCE(stage,'')<>'') "
                "OR (status='ERROR' AND COALESCE(retryable,0)=1 "
                "AND COALESCE(parse_attempts,0)<? AND COALESCE(retry_after,0)<=?)",
                (max_attempts, now),
            )
            return int(dataset_cur.rowcount or 0) + int(cur.rowcount or 0)

    def requeue_repairable_errors(
        self,
        *,
        error_codes: tuple[str, ...] = (
            "SPARSE_VECTOR_PREVALIDATION_MISSING",
            "QDRANT_POINT_COUNT_MISMATCH",
        ),
        max_files: int = 50,
        max_attempts: int | None = None,
    ) -> dict[str, Any]:
        """Requeue a small allowlisted batch after a fixed systemic failure.

        This deliberately excludes module-owned datasets and never scans or
        resets successful documents. The returned counters are persisted for
        diagnostics and the GUI.
        """
        max_files = max(0, min(int(max_files or 0), 500))
        if max_attempts is None:
            try:
                max_attempts = max(1, int(os.getenv("RAG_PARSE_MAX_ATTEMPTS", "4")))
            except ValueError:
                max_attempts = 4
        max_attempts = max(1, int(max_attempts))
        codes = tuple(dict.fromkeys(str(code).strip() for code in error_codes if str(code).strip()))
        now = time.time()
        with self._get_conn() as conn:
            if not codes or max_files == 0:
                eligible = 0
                selected: list[sqlite3.Row] = []
            else:
                placeholders = ",".join("?" for _ in codes)
                where = (
                    "doc.status='ERROR' "
                    f"AND COALESCE(doc.error_code,'') IN ({placeholders}) "
                    "AND COALESCE(doc.parse_attempts,0)<? "
                    "AND COALESCE(ds.module_id,'')=''"
                )
                params: tuple[Any, ...] = (*codes, max_attempts)
                eligible = int(conn.execute(
                    f"SELECT COUNT(*) FROM documents doc JOIN datasets ds ON ds.id=doc.dataset_id WHERE {where}",
                    params,
                ).fetchone()[0] or 0)
                selected = conn.execute(
                    f"SELECT doc.id FROM documents doc JOIN datasets ds ON ds.id=doc.dataset_id "
                    f"WHERE {where} ORDER BY COALESCE(doc.last_attempt_at,0), doc.id LIMIT ?",
                    (*params, max_files),
                ).fetchall()
            ids = [str(row[0]) for row in selected]
            if ids:
                id_placeholders = ",".join("?" for _ in ids)
                conn.execute(
                    "UPDATE documents SET status='PENDING', stage='', retryable=0, retry_after=0 "
                    f"WHERE id IN ({id_placeholders})",
                    ids,
                )
            status = "repaired" if ids else ("eligible_but_disabled" if eligible else "nothing_to_repair")
            conn.execute(
                "UPDATE indexing_repair_state SET ran_at=?, repaired_files=?, eligible_files=?, "
                "max_files=?, status=? WHERE singleton=1",
                (now, len(ids), eligible, max_files, status),
            )
        return {
            "status": status,
            "ran_at": now,
            "repaired_files": len(ids),
            "eligible_files": eligible,
            "remaining_files": max(0, eligible - len(ids)),
            "max_files": max_files,
            "error_codes": list(codes),
        }

    def mark_document_skipped(
        self,
        dataset_id: str,
        file_name: str,
        *,
        message: str,
        error_code: str = "UNSUPPORTED_INDEXING_SOURCE",
    ) -> None:
        with self._get_conn() as conn:
            cur = conn.execute(
                "UPDATE documents SET status='SKIPPED', chunk_count=0, last_error=?, stage='', "
                "error_code=?, retryable=0, retry_after=0 WHERE dataset_id=? AND file_name=?",
                (str(message)[:2000], str(error_code)[:120], dataset_id, file_name),
            )
            if cur.rowcount != 1:
                raise RuntimeError(
                    f"document skip update affected {cur.rowcount} rows "
                    f"for dataset_id={dataset_id}, file_name={file_name}"
                )

    def begin_document_attempt(self, dataset_id: str, file_name: str) -> int:
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE documents SET parse_attempts=COALESCE(parse_attempts,0)+1, "
                "last_attempt_at=?, retryable=0, retry_after=0, error_code='', last_error='' "
                "WHERE dataset_id=? AND file_name=?",
                (time.time(), dataset_id, file_name),
            )
            row = conn.execute(
                "SELECT COALESCE(parse_attempts,0) FROM documents WHERE dataset_id=? AND file_name=?",
                (dataset_id, file_name),
            ).fetchone()
            return int(row[0] or 0) if row else 0

    def mark_document_parse_error(
        self,
        dataset_id: str,
        file_name: str,
        *,
        message: str,
        error_code: str,
        retryable: bool,
        retry_after: float,
    ) -> None:
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE documents SET status='ERROR', chunk_count=0, last_error=?, stage='', "
                "error_code=?, retryable=?, retry_after=? WHERE dataset_id=? AND file_name=?",
                (
                    str(message)[:2000],
                    str(error_code)[:120],
                    1 if retryable else 0,
                    float(retry_after or 0),
                    dataset_id,
                    file_name,
                ),
            )

    def list_datasets(self) -> List[DatasetInfo]:
        from backend.product_edition import is_light
        light = is_light()
        with self._get_conn() as conn:
            if light:
                from backend.smart_index import register_document_visibility
                register_document_visibility(conn, exclude_temporary=True)
            visible_join = (
                "LEFT JOIN documents doc ON d.id = doc.dataset_id "
                "AND les_visible_document(doc.file_name)=1"
                if light else "LEFT JOIN documents doc ON d.id = doc.dataset_id"
            )
            chunks = "COALESCE(SUM(doc.chunk_count), 0)" if light else "d.chunk_count"
            rows = conn.execute(f"""
                SELECT d.id, d.name, d.status, {chunks} AS chunk_count,
                       COALESCE(d.sensitivity, 'P0') AS sensitivity,
                       COALESCE(d.group_name, '') AS group_name,
                       COALESCE(d.dataset_scope, 'user') AS dataset_scope,
                       COALESCE(d.module_id, '') AS module_id,
                       COUNT(doc.id) AS total_files,
                       SUM(CASE WHEN doc.status='INDEXED' THEN 1 ELSE 0 END) AS indexed_files,
                       SUM(CASE WHEN doc.status='PENDING' THEN 1 ELSE 0 END) AS pending_files,
                       SUM(CASE WHEN doc.status='ERROR' THEN 1 ELSE 0 END) AS error_files,
                       SUM(CASE WHEN doc.status='MISSING' THEN 1 ELSE 0 END) AS missing_files
                FROM datasets d
                {visible_join}
                GROUP BY d.id
            """).fetchall()
        return [
            DatasetInfo(
                id=r["id"], name=r["name"], status=r["status"],
                doc_count=r["total_files"] or 0,
                chunk_count=r["chunk_count"] or 0,
                sensitivity=r["sensitivity"] or "P0",
                group_name=r["group_name"] or "",
                files=r["total_files"] or 0,
                indexed_files=r["indexed_files"] or 0,
                pending_files=r["pending_files"] or 0,
                error_files=r["error_files"] or 0,
                missing_files=r["missing_files"] or 0,
                dataset_scope=r["dataset_scope"] or "user",
                module_id=r["module_id"] or "",
            )
            for r in rows if not light or (r["dataset_scope"] or "user") != "system"
        ]

    def set_dataset_sensitivity(self, dataset_id: str, sensitivity: str) -> None:
        """W3.3 (ADR-9): пометить чувствительность датасета (P0/P1/P2)."""
        level = str(sensitivity or "").strip().upper()
        if level not in ("P0", "P1", "P2"):
            raise ValueError(f"sensitivity must be P0/P1/P2, got {sensitivity!r}")
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE datasets SET sensitivity=? WHERE id=?", (level, dataset_id)
            )

    def set_dataset_group(self, dataset_id: str, group_name: str) -> None:
        """Пользовательская группа датасета (организация в САМОВАРе). Пусто = без группы."""
        grp = str(group_name or "").strip()[:60]
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE datasets SET group_name=? WHERE id=?", (grp, dataset_id)
            )

    def set_dataset_name(self, dataset_id: str, name: str) -> None:
        """Переименование датасета."""
        nm = str(name or "").strip()[:120]
        if not nm:
            return
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE datasets SET name=? WHERE id=?", (nm, dataset_id)
            )

    def add_document(
        self, dataset_id: str, file_name: str,
        file_mtime: float = 0.0, file_size: int = 0,
        source_path: str = "",
        *, force_reindex: bool = False,
    ) -> tuple:
        """Возвращает (doc_id, is_new, needs_reindex).

        source_path != "" — внешний in-place источник (абсолютный путь). Документ
        не копируется в storage, а читается из source_path при парсинге.
        """
        with self._get_conn() as conn:
            # Serialize the read/insert pair across threads and processes.
            conn.execute("BEGIN IMMEDIATE")
            existing = conn.execute(
                "SELECT id, file_mtime, file_size, status FROM documents "
                "WHERE dataset_id=? AND file_name=?",
                (dataset_id, file_name),
            ).fetchone()
            if existing:
                doc_id  = existing["id"]
                changed = (
                    force_reindex or (existing["file_mtime"] or 0) != file_mtime
                    or (existing["file_size"] or 0) != file_size
                    or str(existing["status"] or "").upper() == "MISSING"
                )
                if changed:
                    conn.execute(
                        "UPDATE documents SET status='PENDING', file_mtime=?, file_size=?, source_path=? WHERE id=?",
                        (file_mtime, file_size, source_path, doc_id),
                    )
                    return doc_id, False, True
                # Содержимое не изменилось, но абсолютный источник мог переехать — обновим.
                if source_path:
                    conn.execute(
                        "UPDATE documents SET source_path=? WHERE id=?",
                        (source_path, doc_id),
                    )
                return doc_id, False, False
            doc_id = str(uuid.uuid4())
            conn.execute(
                "INSERT INTO documents (id, dataset_id, file_name, status, file_mtime, file_size, source_path) "
                "VALUES (?, ?, ?, 'PENDING', ?, ?, ?)",
                (doc_id, dataset_id, file_name, file_mtime, file_size, source_path),
            )
            return doc_id, True, True

    def update_document_status(
        self,
        dataset_id: str,
        file_name: str,
        status: str,
        chunk_count: int = 0,
        route: DocumentRoute | None = None,
        last_error: str = "",
    ):
        with self._get_conn() as conn:
            fields = ["status=?", "chunk_count=?", "last_error=?", "stage=''"]
            values: list[Any] = [status, chunk_count, last_error[:2000]]
            if route is not None:
                fields.extend([
                    "domain=?",
                    "route_dataset=?",
                    "doc_type=?",
                    "content_type=?",
                    "complexity=?",
                    "pipeline=?",
                ])
                values.extend([
                    route.domain,
                    route.dataset_name,
                    route.doc_type,
                    route.content_type,
                    route.complexity,
                    route.pipeline,
                ])
            values.extend([dataset_id, file_name])
            if status == "INDEXED":
                fields.extend(["retryable=0", "retry_after=0", "error_code=''"])
            cur = conn.execute(
                f"UPDATE documents SET {', '.join(fields)} "
                "WHERE dataset_id=? AND file_name=?",
                values,
            )
            if cur.rowcount != 1:
                raise RuntimeError(
                    f"document status update affected {cur.rowcount} rows "
                    f"for dataset_id={dataset_id}, file_name={file_name}"
                )

    def mark_document_error(self, dataset_id: str, document_id: str, error: str) -> None:
        """Mark one uploaded document as failed by its stable public id."""
        with self._get_conn() as conn:
            cur = conn.execute(
                "UPDATE documents SET status='ERROR', chunk_count=0, last_error=?, stage='' "
                "WHERE dataset_id=? AND id=?",
                (str(error)[:2000], dataset_id, document_id),
            )
            if cur.rowcount != 1:
                raise RuntimeError(
                    f"document error update affected {cur.rowcount} rows "
                    f"for dataset_id={dataset_id}, document_id={document_id}"
                )

    def mark_document_deferred(self, dataset_id: str, document_id: str, reason: str) -> None:
        """A resource gate did not attempt conversion; preserve previous counts."""
        with self._get_conn() as conn:
            cur = conn.execute(
                "UPDATE documents SET status='PENDING', stage='WAITING_RESOURCES', "
                "last_error=?, error_code='PARSE_ADMISSION_DEFERRED', retryable=1, retry_after=0 "
                "WHERE dataset_id=? AND id=?",
                (str(reason)[:2000], dataset_id, document_id),
            )
            if cur.rowcount != 1:
                raise RuntimeError("Deferred document identity not found")

    def requeue_error_documents(self, dataset_id: str) -> int:
        """«Ремонт» датасета: ERROR-документы → PENDING (очистка last_error/stage/chunk_count),
        чтобы перепарсить их БЕЗ удаления датасета/индекса. Возвращает число сброшенных."""
        with self._get_conn() as conn:
            cur = conn.execute(
                "UPDATE documents SET status='PENDING', last_error='', stage='', chunk_count=0 "
                "WHERE dataset_id=? AND status='ERROR'",
                (dataset_id,),
            )
            return cur.rowcount

    def requeue_corrupt_pdf_text_documents(self, dataset_id: str) -> list[str]:
        """Find already indexed PDF text damaged by UTF-8/Latin-1 mojibake and requeue its source.

        Detection is based on the same conservative normalizer used by new PDF
        ingestion.  A document is touched only when at least two chunks are
        repairable and at least a quarter of its indexed chunks are affected.
        """
        with self._get_conn() as conn:
            table = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='lexical_chunks'"
            ).fetchone()
            if not table:
                return []
            rows = conn.execute(
                "SELECT doc_name, text FROM lexical_chunks WHERE dataset_id=? ORDER BY doc_name, id",
                (dataset_id,),
            ).fetchall()
            totals: dict[str, int] = {}
            damaged: dict[str, int] = {}
            for row in rows:
                name = str(row["doc_name"] or "")
                if not name.lower().endswith((".pdf", ".p7m")):
                    continue
                text = str(row["text"] or "")
                totals[name] = totals.get(name, 0) + 1
                if normalize_pdf_text(text) != text:
                    damaged[name] = damaged.get(name, 0) + 1
            names = sorted(
                name for name, count in damaged.items()
                if count >= 2 and count * 4 >= totals.get(name, 0)
            )
            if not names:
                return []
            placeholders = ",".join("?" for _ in names)
            conn.execute(
                f"UPDATE documents SET status='PENDING', last_error='', stage='', chunk_count=0 "
                f"WHERE dataset_id=? AND file_name IN ({placeholders})",
                (dataset_id, *names),
            )
            return names

    def update_document_stage(self, dataset_id: str, file_name: str, stage: str) -> None:
        """W1.4: текущая стадия конвейера файла (CONVERT/EMBED/UPSERT) — для прогресса/диагностики."""
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE documents SET stage=? WHERE dataset_id=? AND file_name=?",
                (stage, dataset_id, file_name),
            )

    def dataset_parse_progress(self, dataset_id: str) -> dict[str, Any]:
        """Small read-only snapshot for the operator job poller."""
        with self._get_conn() as conn:
            conn.row_factory = sqlite3.Row
            counts = conn.execute(
                """
                SELECT
                    COUNT(*) AS total,
                    SUM(CASE WHEN status='INDEXED' THEN 1 ELSE 0 END) AS indexed,
                    SUM(CASE WHEN status='PENDING' THEN 1 ELSE 0 END) AS pending,
                    SUM(CASE WHEN status='ERROR' THEN 1 ELSE 0 END) AS errors
                FROM documents WHERE dataset_id=?
                """,
                (dataset_id,),
            ).fetchone()
            active = conn.execute(
                """
                SELECT file_name, stage
                FROM documents
                WHERE dataset_id=? AND status='PENDING' AND COALESCE(stage, '')<>''
                ORDER BY file_name
                LIMIT 1
                """,
                (dataset_id,),
            ).fetchone()
        return {
            "total": int(counts["total"] or 0),
            "indexed": int(counts["indexed"] or 0),
            "pending": int(counts["pending"] or 0),
            "errors": int(counts["errors"] or 0),
            "file_name": str(active["file_name"] or "") if active else "",
            "stage": str(active["stage"] or "") if active else "",
        }

    def update_document_route(self, dataset_id: str, file_name: str, route: DocumentRoute):
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE documents SET domain=?, route_dataset=?, doc_type=?, content_type=?, complexity=?, pipeline=? "
                "WHERE dataset_id=? AND file_name=?",
                (
                    route.domain,
                    route.dataset_name,
                    route.doc_type,
                    route.content_type,
                    route.complexity,
                    route.pipeline,
                    dataset_id,
                    file_name,
                ),
            )

    def update_dataset_chunk_count(self, dataset_id: str):
        with self._get_conn() as conn:
            row = conn.execute(
                "SELECT COALESCE(SUM(chunk_count),0) as total FROM documents "
                "WHERE dataset_id=? AND status='INDEXED'",
                (dataset_id,),
            ).fetchone()
            conn.execute(
                "UPDATE datasets SET chunk_count=? WHERE id=?",
                (row["total"] if row else 0, dataset_id),
            )

    def apply_document_chunk_count_repairs(
        self,
        repairs: list[tuple[str, str, int]],
    ) -> int:
        """Atomically repair only INDEXED document counters and affected dataset totals."""
        if not repairs:
            return 0
        affected: set[str] = set()
        updated = 0
        with self._get_conn() as conn:
            conn.execute("BEGIN IMMEDIATE")
            for dataset_id, file_name, chunk_count in repairs:
                cur = conn.execute(
                    "UPDATE documents SET chunk_count=? "
                    "WHERE dataset_id=? AND file_name=? AND status='INDEXED'",
                    (max(0, int(chunk_count)), dataset_id, file_name),
                )
                if int(cur.rowcount or 0):
                    updated += 1
                    affected.add(dataset_id)
            for dataset_id in sorted(affected):
                conn.execute(
                    "UPDATE datasets SET chunk_count=("
                    "SELECT COALESCE(SUM(chunk_count),0) FROM documents "
                    "WHERE dataset_id=? AND status='INDEXED') WHERE id=?",
                    (dataset_id, dataset_id),
                )
            conn.commit()
        return updated

    def get_pending_files(self, dataset_id: str, limit: int | None = None) -> List[str]:
        sql = (
            "SELECT file_name FROM documents WHERE dataset_id=? AND status='PENDING' "
            "ORDER BY "
            "CASE WHEN complexity='needs_ocr' OR pipeline='markdown_needs_ocr' THEN 1 ELSE 0 END, "
            "COALESCE(NULLIF(file_size, 0), 9223372036854775807), file_name"
        )
        params: list[Any] = [dataset_id]
        if limit is not None:
            sql += " LIMIT ?"
            params.append(max(0, int(limit)))
        with self._get_conn() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [r["file_name"] for r in rows]

    def indexed_files_with_counts(self, dataset_id: str) -> List[tuple[str, int]]:
        """INDEXED-документы датасета с их chunk_count — для сверки с Qdrant (reconcile)."""
        with self._get_conn() as conn:
            rows = conn.execute(
                "SELECT file_name, COALESCE(chunk_count, 0) AS cc FROM documents "
                "WHERE dataset_id=? AND status='INDEXED'",
                (dataset_id,),
            ).fetchall()
        return [(r["file_name"], int(r["cc"])) for r in rows]

    def dataset_integrity_rows(self, dataset_id: str) -> list[dict[str, Any]]:
        """Source and index metadata used by the explicit dataset integrity audit."""
        with self._get_conn() as conn:
            rows = conn.execute(
                "SELECT id, file_name, status, COALESCE(file_hash, '') AS file_hash, "
                "COALESCE(file_mtime, 0) AS file_mtime, COALESCE(file_size, 0) AS file_size, "
                "COALESCE(chunk_count, 0) AS chunk_count, COALESCE(source_path, '') AS source_path "
                "FROM documents WHERE dataset_id=? ORDER BY file_name",
                (dataset_id,),
            ).fetchall()
        return [dict(row) for row in rows]

    def set_document_source_fingerprint(
        self,
        dataset_id: str,
        file_name: str,
        *,
        file_hash: str,
        file_mtime: float,
        file_size: int,
    ) -> None:
        with self._get_conn() as conn:
            conn.execute(
                "UPDATE documents SET file_hash=?, file_mtime=?, file_size=? "
                "WHERE dataset_id=? AND file_name=?",
                (file_hash, file_mtime, file_size, dataset_id, file_name),
            )

    def lexical_integrity_projection(self, dataset_id: str) -> dict[str, Any]:
        """Return exact lexical/FTS point ids by file without invoking retrieval."""
        with self._get_conn() as conn:
            lexical_exists = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE name='lexical_chunks'"
            ).fetchone()
            fts_exists = conn.execute(
                "SELECT 1 FROM sqlite_master WHERE name='lexical_chunks_fts'"
            ).fetchone()
            if not lexical_exists:
                return {"available": False, "fts_available": bool(fts_exists), "files": {}, "fts_ids": set()}
            rows = conn.execute(
                "SELECT doc_name, point_id FROM lexical_chunks "
                "WHERE collection=? AND dataset_id=?",
                (rag_collection_name(), dataset_id),
            ).fetchall()
            files: dict[str, set[str]] = {}
            for row in rows:
                files.setdefault(str(row["doc_name"] or ""), set()).add(str(row["point_id"] or ""))
            fts_ids: set[int] = set()
            if fts_exists:
                fts_ids = {
                    int(row["id"])
                    for row in conn.execute(
                        "SELECT c.id FROM lexical_chunks c "
                        "JOIN lexical_chunks_fts f ON f.rowid=c.id "
                        "WHERE c.collection=? AND c.dataset_id=?",
                        (rag_collection_name(), dataset_id),
                    ).fetchall()
                }
            lexical_ids = {
                int(row["id"])
                for row in conn.execute(
                    "SELECT id FROM lexical_chunks WHERE collection=? AND dataset_id=?",
                    (rag_collection_name(), dataset_id),
                ).fetchall()
            }
        return {
            "available": True,
            "fts_available": bool(fts_exists),
            "files": files,
            "lexical_ids": lexical_ids,
            "fts_ids": fts_ids,
        }

    def rebuild_lexical_fts(self) -> None:
        from proxy.services.lexical_index_service import LexicalIndex

        with LexicalIndex(self.db_path).connect() as conn:
            conn.execute("INSERT INTO lexical_chunks_fts(lexical_chunks_fts) VALUES('rebuild')")

    def set_documents_pending(self, dataset_id: str, file_names: set[str]) -> int:
        if not file_names:
            return 0
        names = sorted(file_names)
        placeholders = ",".join("?" for _ in names)
        with self._get_conn() as conn:
            cur = conn.execute(
                f"UPDATE documents SET status='PENDING', last_error='', stage='', chunk_count=0 "
                f"WHERE dataset_id=? AND file_name IN ({placeholders})",
                (dataset_id, *names),
            )
            return int(cur.rowcount)

    def set_documents_missing(self, dataset_id: str, file_names: set[str]) -> int:
        if not file_names:
            return 0
        names = sorted(file_names)
        placeholders = ",".join("?" for _ in names)
        with self._get_conn() as conn:
            cur = conn.execute(
                f"UPDATE documents SET status='MISSING', last_error='Исходный файл не найден', "
                f"stage='', chunk_count=0 WHERE dataset_id=? AND file_name IN ({placeholders})",
                (dataset_id, *names),
            )
            return int(cur.rowcount)

    def get_pending_files_with_paths(
        self, dataset_id: str, limit: int | None = None
    ) -> List[tuple]:
        """Как get_pending_files, но (file_name, source_path).

        source_path != "" — внешний in-place источник; _sync_parse читает его по
        абсолютному пути вместо storage/datasets/{id}/{file_name}.
        """
        sql = (
            "SELECT file_name, COALESCE(source_path, '') AS source_path FROM documents "
            "WHERE dataset_id=? AND status='PENDING' "
            "ORDER BY "
            "CASE WHEN complexity='needs_ocr' OR pipeline='markdown_needs_ocr' THEN 1 ELSE 0 END, "
            "COALESCE(NULLIF(file_size, 0), 9223372036854775807), file_name"
        )
        params: list[Any] = [dataset_id]
        if limit is not None:
            sql += " LIMIT ?"
            params.append(max(0, int(limit)))
        with self._get_conn() as conn:
            rows = conn.execute(sql, params).fetchall()
        return [(r["file_name"], r["source_path"]) for r in rows]

    def health_snapshot(self) -> Dict[str, Any]:
        with self._get_conn() as conn:
            dataset_rows = conn.execute("""
                SELECT d.id, d.name, d.status, d.chunk_count,
                       COALESCE(d.dataset_scope, 'user') AS dataset_scope,
                       COALESCE(d.module_id, '') AS module_id,
                       COUNT(doc.id) AS total_files,
                       SUM(CASE WHEN doc.status='INDEXED' THEN 1 ELSE 0 END) AS indexed_files,
                       SUM(CASE WHEN doc.status='PENDING' THEN 1 ELSE 0 END) AS pending_files,
                       SUM(CASE WHEN doc.status='ERROR' THEN 1 ELSE 0 END) AS error_files,
                       SUM(CASE WHEN doc.status='SKIPPED' THEN 1 ELSE 0 END) AS skipped_files,
                       SUM(CASE WHEN doc.status='MISSING' THEN 1 ELSE 0 END) AS missing_files,
                       COALESCE(SUM(CASE WHEN doc.status='INDEXED' THEN doc.chunk_count ELSE 0 END), 0) AS indexed_chunks
                FROM datasets d
                LEFT JOIN documents doc ON d.id = doc.dataset_id
                GROUP BY d.id
                ORDER BY d.name
            """).fetchall()
            status_rows = conn.execute(
                "SELECT status, COUNT(*) AS files, COALESCE(SUM(chunk_count),0) AS chunks "
                "FROM documents GROUP BY status"
            ).fetchall()
            route_rows = conn.execute("""
                SELECT COALESCE(NULLIF(domain, ''), 'UNCLASSIFIED') AS domain,
                       COUNT(*) AS files,
                       COALESCE(SUM(chunk_count),0) AS chunks
                FROM documents
                GROUP BY COALESCE(NULLIF(domain, ''), 'UNCLASSIFIED')
                ORDER BY files DESC
            """).fetchall()
            doc_type_rows = conn.execute("""
                SELECT COALESCE(NULLIF(doc_type, ''), 'UNCLASSIFIED') AS doc_type,
                       COUNT(*) AS files,
                       COALESCE(SUM(chunk_count),0) AS chunks
                FROM documents
                GROUP BY COALESCE(NULLIF(doc_type, ''), 'UNCLASSIFIED')
                ORDER BY files DESC
            """).fetchall()
            retry_rows = conn.execute("""
                SELECT COALESCE(NULLIF(error_code, ''), 'UNCLASSIFIED') AS error_code,
                       COUNT(*) AS files,
                       SUM(CASE WHEN COALESCE(retryable,0)=1 THEN 1 ELSE 0 END) AS retryable_files,
                       MAX(COALESCE(parse_attempts,0)) AS max_attempts,
                       MIN(CASE WHEN COALESCE(retryable,0)=1 THEN NULLIF(retry_after,0) END) AS next_retry_at
                FROM documents
                WHERE status='ERROR'
                GROUP BY COALESCE(NULLIF(error_code, ''), 'UNCLASSIFIED')
                ORDER BY files DESC, error_code
            """).fetchall()
            skipped_rows = conn.execute("""
                SELECT COALESCE(NULLIF(error_code, ''), 'UNCLASSIFIED') AS error_code,
                       COUNT(*) AS files
                FROM documents
                WHERE status='SKIPPED'
                GROUP BY COALESCE(NULLIF(error_code, ''), 'UNCLASSIFIED')
                ORDER BY files DESC, error_code
            """).fetchall()
            repair_row = conn.execute(
                "SELECT ran_at, repaired_files, eligible_files, max_files, status "
                "FROM indexing_repair_state WHERE singleton=1"
            ).fetchone()

        datasets = [
            {
                "id": row["id"],
                "name": row["name"],
                "status": row["status"],
                "files": row["total_files"] or 0,
                "indexed_files": row["indexed_files"] or 0,
                "pending_files": row["pending_files"] or 0,
                "error_files": row["error_files"] or 0,
                "skipped_files": row["skipped_files"] or 0,
                "missing_files": row["missing_files"] or 0,
                "chunks": row["indexed_chunks"] or 0,
                "dataset_scope": row["dataset_scope"] or "user",
                "module_id": row["module_id"] or "",
            }
            for row in dataset_rows
        ]
        totals = {
            "datasets": len(datasets),
            "files": sum(item["files"] for item in datasets),
            "indexed_files": sum(item["indexed_files"] for item in datasets),
            "pending_files": sum(item["pending_files"] for item in datasets),
            "error_files": sum(item["error_files"] for item in datasets),
            "skipped_files": sum(item["skipped_files"] for item in datasets),
            "missing_files": sum(item["missing_files"] for item in datasets),
            "chunks": sum(item["chunks"] for item in datasets),
        }
        return {
            "status": self._rag_status(totals, datasets),
            "totals": totals,
            "by_status": {
                row["status"]: {"files": row["files"], "chunks": row["chunks"]}
                for row in status_rows
            },
            "by_domain": {
                row["domain"]: {"files": row["files"], "chunks": row["chunks"]}
                for row in route_rows
            },
            "by_doc_type": {
                row["doc_type"]: {"files": row["files"], "chunks": row["chunks"]}
                for row in doc_type_rows
            },
            "indexing_recovery": {
                "skipped_files": totals["skipped_files"],
                "retryable_errors": sum(int(row["retryable_files"] or 0) for row in retry_rows),
                "terminal_errors": sum(
                    int(row["files"] or 0) - int(row["retryable_files"] or 0)
                    for row in retry_rows
                ),
                "next_retry_at": min(
                    (float(row["next_retry_at"]) for row in retry_rows if row["next_retry_at"] is not None),
                    default=0.0,
                ),
                "by_error_code": {
                    row["error_code"]: {
                        "files": int(row["files"] or 0),
                        "retryable_files": int(row["retryable_files"] or 0),
                        "max_attempts": int(row["max_attempts"] or 0),
                    }
                    for row in retry_rows
                },
                "skipped_by_error_code": {
                    row["error_code"]: {"files": int(row["files"] or 0), "disposition": "skipped"}
                    for row in skipped_rows
                },
                "bounded_repair": {
                    "status": str(repair_row["status"] or "never") if repair_row else "never",
                    "ran_at": float(repair_row["ran_at"] or 0) if repair_row else 0.0,
                    "repaired_files": int(repair_row["repaired_files"] or 0) if repair_row else 0,
                    "eligible_files": int(repair_row["eligible_files"] or 0) if repair_row else 0,
                    "max_files": int(repair_row["max_files"] or 0) if repair_row else 0,
                },
            },
            "datasets": datasets,
        }

    def _rag_status(self, totals: Dict[str, int], datasets: list[dict]) -> str:
        if totals["files"] == 0:
            return "empty"
        if totals["indexed_files"] == 0:
            return "not_indexed"
        if totals["pending_files"] or totals["error_files"] or totals.get("missing_files", 0):
            return "degraded"
        if any(dataset["status"] not in ("COMPLETED", "IDLE") for dataset in datasets):
            return "degraded"
        return "ready"

    def insert_structured_rules(self, rules: List[Dict[str, Any]]) -> None:
        if not rules:
            return
        with self._get_conn() as conn:
            conn.executemany("""
                INSERT OR REPLACE INTO structured_rules (
                    id, document_id, file_key, chunk_id, subject, parameter, operator, value, unit, condition, char_start, char_end
                ) VALUES (
                    :id, :document_id, :file_key, :chunk_id, :subject, :parameter, :operator, :value, :unit, :condition, :char_start, :char_end
                )
            """, rules)

    def get_structured_rules(self, document_id: Optional[str] = None, file_key: Optional[str] = None) -> List[sqlite3.Row]:
        query = "SELECT * FROM structured_rules WHERE 1=1"
        params = []
        if document_id:
            query += " AND document_id = ?"
            params.append(document_id)
        if file_key:
            query += " AND file_key = ?"
            params.append(file_key)
        
        with self._get_conn() as conn:
            return conn.execute(query, params).fetchall()

    def clear_structured_rules(self, file_key: str) -> None:
        with self._get_conn() as conn:
            conn.execute("DELETE FROM structured_rules WHERE file_key = ?", (file_key,))
