"""Chat request validation and candidate acceptance boundary."""
from __future__ import annotations
import logging
import re
from typing import Any, List, Optional
from fastapi import HTTPException
from pydantic import BaseModel, field_validator
from proxy.services.chat_provider_session_service import ChatProviderConfig
from proxy.services.candidate_acceptance_service import (
    CandidateAcceptanceError,
    require_candidate_acceptance,
)

logger = logging.getLogger(__name__)

class ChatRequest(BaseModel):
    question: str
    dataset_ids: Optional[List[str]] = None
    dataset_filter: Optional[str] = None
    # Explicit per-turn opt-in from the chat checkbox; omitted/null means off.
    reranker_enabled: Optional[bool] = None
    semantic_cache_enabled: Optional[bool] = None
    validation_enabled: Optional[bool] = None
    session_id: Optional[str] = None
    project_id: Optional[int] = None  # W17.1: режим проекта — ретрив сужается к датасетам объекта
    scope: Optional[dict] = None  # v0.21: нормализованная область поиска {scope_type, project_ids, dataset_ids}
    selected_sources_only: Optional[bool] = None
    output_directive: Optional[str] = None  # формат/стиль ответа — ТОЛЬКО в генерацию (не в роутинг/заметки/ретрив)
    response_length: Optional[str] = None  # short|standard|detailed|maximum; только бюджет/форма генерации
    mode: Optional[str] = None  # явный режим чата из UI
    profile_revision_id: Optional[str] = None
    apply_profile_revision: bool = False
    candidate_acceptance: bool = False  # root-admin only; isolated pre-promotion execution
    attachment_context: Optional[str] = None  # текст файла из скрепки (read-mode), без индексации
    attachment_id: Optional[str] = None  # server-owned read_<id>; клиентский путь не принимается
    target_file: Optional[str] = None  # точный file_name из MetaDB documents (для клика по реестру/узкого RAG)
    target_files: Optional[List[str]] = None  # явный выбор нескольких документов оператором
    provider_config: Optional[ChatProviderConfig] = None  # per-session BYOK, без изменения общего .env

    @field_validator("question")
    @classmethod
    def question_limits(cls, v):
        v = v.strip()
        if not v:
            raise ValueError("Пустой вопрос")
        # Сметные исходники часто приходят как pasted ВОР/спецификация, а не как
        # отдельный attachment_context. 4k ломал живой сценарий "спецификация -> ВОР".
        if len(v) > 20000:
            raise ValueError(f"Вопрос слишком длинный ({len(v)} симв., макс. 20000)")
        return v

    @field_validator("attachment_context")
    @classmethod
    def attachment_context_limits(cls, v):
        if v is None:
            return None
        v = v.strip()
        if not v:
            return None
        if len(v) > 20000:
            raise ValueError(f"Контекст вложения слишком длинный ({len(v)} симв., макс. 20000)")
        return v

    @field_validator("attachment_id")
    @classmethod
    def attachment_id_limits(cls, v):
        if v is None:
            return None
        value = v.strip().lower()
        if not re.fullmatch(r"read_[0-9a-f]{12}", value):
            raise ValueError("Некорректный идентификатор вложения")
        return value

    @field_validator("target_file")
    @classmethod
    def target_file_limits(cls, v):
        if v is None:
            return None
        v = v.strip().replace("\\", "/")
        if not v:
            return None
        if len(v) > 1000:
            raise ValueError(f"Имя целевого файла слишком длинное ({len(v)} симв., макс. 1000)")
        return v

    @field_validator("response_length")
    @classmethod
    def response_length_values(cls, v):
        if v is None:
            return None
        value = v.strip().casefold()
        if value not in {"short", "standard", "detailed", "maximum"}:
            raise ValueError("Некорректная длина ответа")
        return value

    @field_validator("target_files")
    @classmethod
    def target_files_limits(cls, values):
        if values is None:
            return None
        result: list[str] = []
        for raw in values:
            value = str(raw or "").strip().replace("\\", "/")
            if not value or value in result:
                continue
            if len(value) > 1000:
                raise ValueError("Имя выбранного документа слишком длинное")
            result.append(value)
        return result or None


def _require_candidate_acceptance(req: ChatRequest, user: Any) -> None:
    try:
        require_candidate_acceptance(
            requested=bool(req.candidate_acceptance),
            user=user,
        )
    except CandidateAcceptanceError as error:
        status_code = 403 if "ROOT_ADMIN" in str(error) else 409
        raise HTTPException(status_code, str(error)) from error
