"""Request contracts and limits for dataset operations."""
from __future__ import annotations
import logging
import os
import re
from backend.runtime_paths import mutable_path
from pydantic import BaseModel, Field


logger = logging.getLogger(__name__)


UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.I)


DEFAULT_PARSE_BATCH_LIMIT = int(os.getenv("RAG_PARSE_BATCH_LIMIT", "5"))


DEFAULT_PARSE_SCHEDULER_BATCH_LIMIT = int(os.getenv("RAG_PARSE_SCHEDULER_BATCH_LIMIT", "1"))


DEFAULT_PARSE_SCHEDULER_MAX_BATCHES = int(os.getenv("RAG_PARSE_SCHEDULER_MAX_BATCHES", "25"))


DEFAULT_PARSE_DRAIN_MAX_BATCHES = int(os.getenv("RAG_PARSE_DRAIN_MAX_BATCHES", "500"))


PARSE_MIN_FREE_GB = float(os.getenv("RAG_PARSE_MIN_FREE_GB", "2.5"))


PARSE_MAX_SWAP_PCT = float(os.getenv("RAG_PARSE_MAX_SWAP_PCT", "45"))


PARSE_POST_MAX_SWAP_PCT = float(os.getenv("RAG_PARSE_POST_MAX_SWAP_PCT", "60"))


ACTIVE_PARSE_SCHEDULER_STATUSES = {"QUEUED", "PARSING", "RUNNING"}


FOLDER_WATCH_CACHE_TTL_SEC = float(os.getenv("RAG_WATCH_CACHE_TTL_SEC", "15"))


FOLDER_WATCH_CACHE_SAMPLE_LIMIT = int(os.getenv("RAG_WATCH_CACHE_SAMPLE_LIMIT", "200"))


_TRUE_ENV_VALUES = {"1", "true", "yes", "on"}


class RetrievalDebugRequest(BaseModel):
    reranker_enabled: bool = False
    question: str = Field(min_length=1, max_length=4000)
    dataset_ids: list[str] | None = None
    dataset_filter: str | None = None
    top_k: int = Field(default=8, ge=1, le=20)


class SearchRequest(BaseModel):
    reranker_enabled: bool = False
    query: str | None = Field(default=None, min_length=1, max_length=4000)
    question: str | None = Field(default=None, min_length=1, max_length=4000)
    dataset_ids: list[str] | None = None
    dataset_filter: str | None = None
    top_k: int = Field(default=8, ge=1, le=50)
    max_chars: int = Field(default=1600, ge=200, le=8000)
    include_trace: bool = False
    include_context: bool = False

    def effective_query(self) -> str:
        value = self.query or self.question or ""
        return value.strip()


class SmartSyncRequest(BaseModel):
    source_root: str = "RAG_Content"
    parse: bool = False
    parse_limit_per_dataset: int = Field(default=DEFAULT_PARSE_BATCH_LIMIT, ge=1, le=25)


class FolderWatchRequest(BaseModel):
    source_root: str = "RAG_Content"
    limit: int = Field(default=20, ge=1, le=200)


class IndexExternalRequest(BaseModel):
    path: str = Field(min_length=1)
    dataset_id: str = Field(min_length=1)
    parse: bool = True
    parse_limit: int = Field(default=25, ge=1, le=500)
    auto_split: bool = False  # Legacy input accepted; external sources remain read-only.
    split_max_mb: float = Field(default=40.0, ge=5, le=500)
    background: bool = False  # True → регистрация+нарезка+парс в фоне, мгновенный ответ (большие папки)


class ExternalIntakePlanRequest(BaseModel):
    path: str = Field(min_length=1)
    dataset_name: str = Field(min_length=1, max_length=160)
    project_name: str = Field(default="", max_length=160)


EXTERNAL_SERVICE_FILENAMES = {"LES.md", "ЛЕС.md", "les.md", "лес.md", "00_dataset_map.md"}


class ExternalDatasetSyncRequest(BaseModel):
    path: str = Field(min_length=1)
    dataset_id: str = Field(min_length=1)
    parse: bool = True
    parse_limit: int = Field(default=25, ge=1, le=500)
    include_deleted: bool = True
    limit: int = Field(default=50, ge=1, le=500)


class DatasetWatchRequest(BaseModel):
    path: str = Field(min_length=1)
    enabled: bool = True
    auto_index: bool = False


class CloudDriveListRequest(BaseModel):
    provider: str = Field(pattern="^(google_drive|yandex_disk)$")
    locator: str = Field(default="", max_length=2000)
    limit: int = Field(default=200, ge=1, le=1000)


class CloudDriveSyncRequest(BaseModel):
    provider: str = Field(pattern="^(google_drive|yandex_disk)$")
    locator: str = Field(min_length=1, max_length=2000)
    dataset_id: str = Field(default="", max_length=160)
    dataset_name: str = Field(default="", max_length=160)
    parse: bool = True
    parse_limit: int = Field(default=25, ge=1, le=500)
    max_files: int = Field(default=500, ge=1, le=5000)
    max_depth: int = Field(default=6, ge=0, le=20)
    background: bool = False


class ParseSchedulerRequest(BaseModel):
    batch_limit: int = Field(default=DEFAULT_PARSE_SCHEDULER_BATCH_LIMIT, ge=1, le=25)
    max_batches: int = Field(default=DEFAULT_PARSE_SCHEDULER_MAX_BATCHES, ge=1, le=50000)
    cooldown_sec: float = Field(default=20.0, ge=0, le=600)
    min_free_gb: float | None = Field(default=None, ge=1, le=64)
    max_swap_pct: float | None = Field(default=None, ge=0, le=100)
    post_batch_min_free_gb: float | None = Field(default=None, ge=1, le=64)
    post_batch_max_swap_pct: float | None = Field(default=None, ge=0, le=100)
    stop_on_error: bool = False
    background: bool = True
    dataset_priority_order: list[str] | None = None


class DatasetProfileWarmupRequest(BaseModel):
    dataset_ids: list[str] | None = None
    depth: str = "deep"
    force: bool = False
    limit: int = Field(default=0, ge=0, le=500)


class DatasetGuidanceRequest(BaseModel):
    guidance: str = Field(default="", max_length=4000)
    depth: str = "deep"


class DatasetKindRequest(BaseModel):
    kind: str = Field(default="", max_length=40)
    depth: str = "deep"


_PARSE_STAGE_LABELS = {
    "CONVERT": "чтение страниц",
    "EMBED": "создание поискового индекса",
    "UPSERT": "сохранение индекса",
}


class CreateDatasetRequest(BaseModel):
    name: str = ""


class DatasetGroupPayload(BaseModel):
    group: str = ""
    group_name: str = ""


class DatasetNamePayload(BaseModel):
    name: str = ""


_EXTRACT_STORAGE_ROOT = mutable_path("storage/datasets")


_CHAT_ATTACH_DATASET_NAME = "Вложения чата"


class ChatFolderRead(BaseModel):
    path: str = Field(min_length=1, max_length=32767)
