"""Compatibility dispatcher for the native trusted execution policy and SQLite ledger."""
from __future__ import annotations
import asyncio
from collections.abc import Callable, Mapping
from dataclasses import dataclass, asdict
import inspect
from pathlib import Path
import time
from types import MappingProxyType
from typing import Any
from backend import native_core
from proxy.services.tool_contract_service import ToolContract, EffectClass
from proxy.services.tool_registry_service import ToolRegistry, ToolRegistration


def _native(action, **fields):
    result = native_core.required('tool_execution', request={
        'schema': 'les.tool-execution.v1', 'action': action, **fields})
    if result.get('ok') is not True:
        raise RuntimeError(result.get('code') or 'TOOL_EXECUTION_STATE_ERROR')
    return result['value']


def _freeze(value: Any) -> Any:
    if isinstance(value, Mapping):
        return MappingProxyType({str(key): _freeze(item) for key, item in value.items()})
    if isinstance(value, (list, tuple)):
        return tuple(_freeze(item) for item in value)
    return value


def _public(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _public(item) for key, item in value.items()}
    if isinstance(value, tuple):
        return [_public(item) for item in value]
    return value


def argument_sha256(arguments: Mapping[str, Any]) -> str:
    return _native('hash', arguments=_public(arguments))['sha256']


@dataclass(frozen=True)
class ExecutionRequest:
    call_id: str
    tool_name: str
    arguments: Mapping[str, Any]
    allowed_dataset_ids: tuple[str, ...]
    actor_id: str
    actor_role: str
    approval_receipt_id: str | None
    idempotency_key: str | None
    deadline_monotonic: float
    shadow: bool = False
    allowed_attachment_ids: tuple[str, ...] = ()


@dataclass(frozen=True)
class ExecutionEnvelope:
    schema: str
    call_id: str
    tool_name: str
    status: str
    code: str
    result: Mapping[str, Any]
    cursor: str | None
    omitted_items: int

    def to_dict(self) -> dict[str, Any]:
        return {
            "schema": self.schema,
            "call_id": self.call_id,
            "tool_name": self.tool_name,
            "status": self.status,
            "code": self.code,
            "result": _public(self.result),
            "cursor": self.cursor,
            "omitted_items": self.omitted_items,
        }

    def metadata(self) -> dict[str, Any]:
        payload = self.to_dict()
        payload.pop("result", None)
        return payload


@dataclass(frozen=True)
class ApprovalReceipt:
    receipt_id: str
    status: str
    proposal_revision: str
    tool_name: str
    argument_sha256: str
    actor_id: str
    expires_at_epoch: float


class SqliteExecutionStore:
    """Rust owns the existing durable schema and transactions."""
    def __init__(self, path: str | Path):
        self.path = str(Path(path).resolve())
        self._call('ensure')

    def _call(self, action, **fields):
        return _native('store', database=self.path, store_action=action, arguments=_public(fields))

    def issue_approval(self, receipt: ApprovalReceipt):
        self._call('issue', receipt=asdict(receipt))

    def approval(self, receipt_id):
        value = self._call('approval', receipt_id=receipt_id)
        return ApprovalReceipt(**value) if value is not None else None

    def revoke_approval(self, receipt_id):
        return self._call('revoke', receipt_id=receipt_id)

    def put_cursor(self, *, cursor, actor_id, expires_at_epoch, result):
        self._call('put_cursor', cursor=cursor, actor_id=actor_id, expires_at_epoch=expires_at_epoch, result=result)

    def resolve_cursor(self, cursor, *, actor_id):
        return self._call('resolve_cursor', cursor=cursor, actor_id=actor_id)

    def claim(self, **identity):
        value = self._call('claim', **identity)
        return value['state'], _envelope_from_payload(value['envelope']) if value.get('envelope') is not None else None

    def complete(self, *, envelope, **identity):
        self._call('complete', envelope=envelope.to_dict(), **identity)

    def mark_ambiguous(self, **identity):
        self._call('ambiguous', **identity)


def _envelope_from_payload(payload: Mapping[str, Any]) -> ExecutionEnvelope:
    return ExecutionEnvelope(
        schema=str(payload["schema"]),
        call_id=str(payload["call_id"]),
        tool_name=str(payload["tool_name"]),
        status=str(payload["status"]),
        code=str(payload["code"]),
        result=_freeze(payload.get("result") or {}),
        cursor=str(payload["cursor"]) if payload.get("cursor") else None,
        omitted_items=int(payload.get("omitted_items") or 0),
    )


class TrustedExecutor:
    """Handlers are adapters; Rust admits and finishes every invocation."""
    def __init__(self, registry: ToolRegistry, *, store: SqliteExecutionStore | None = None,
                 cursor_ttl_seconds=300, cursor_capacity=128, scope_resolver=None,
                 local_state_authorizer=None):
        self.registry = registry
        self.store = store
        self._scope_resolver = scope_resolver
        self.local_state_authorizer = local_state_authorizer
        self._native_handle = _native('new', database=store.path if store else None,
            cursor_ttl_seconds=cursor_ttl_seconds, cursor_capacity=cursor_capacity)['handle']

    def close(self):
        handle = getattr(self, '_native_handle', None)
        if handle is not None:
            self._native_handle = None
            _native('close', handle=handle)

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass

    def _call(self, action, **fields):
        return _native(action, handle=self._native_handle, **fields)

    def _envelope(self, request, status, code):
        return ExecutionEnvelope('les_tool_execution_v1', request.call_id, request.tool_name,
                                 status, code, _freeze({}), None, 0)

    def _finish(self, ticket, *, raw=None, failure=None):
        return _envelope_from_payload(self._call('finish', ticket=ticket, raw=raw, failure=failure)['envelope'])

    async def execute(self, request: ExecutionRequest) -> ExecutionEnvelope:
        registration = self.registry.get(request.tool_name)
        contract = registration.contract if registration is not None else None
        resolved = ()
        local_allowed = False
        try:
            if (contract is not None and 'dataset' in contract.scopes and
                    '*' not in request.allowed_dataset_ids and self._scope_resolver is not None and
                    isinstance(request.arguments, Mapping)):
                resolved = tuple(str(item) for item in self._scope_resolver(contract, request.arguments) if str(item))
        except Exception:
            return self._envelope(request, 'rejected', 'TOOL_SCOPE_VIOLATION')
        if contract is not None and contract.effect is EffectClass.LOCAL_STATE:
            try:
                local_allowed = self.local_state_authorizer is not None and bool(self.local_state_authorizer(request))
            except Exception:
                local_allowed = False
        fields = {name: _public(getattr(request, name)) for name in (
            'call_id', 'tool_name', 'arguments', 'allowed_dataset_ids', 'actor_id', 'actor_role',
            'approval_receipt_id', 'idempotency_key', 'shadow', 'allowed_attachment_ids')}
        try:
            admitted = self._call('begin', request=fields,
                contract=contract.public_payload() if contract is not None else None,
                admission={'remaining_seconds': request.deadline_monotonic-time.monotonic(),
                    'resolved_dataset_ids': resolved, 'local_state_allowed': local_allowed})
        except (RuntimeError, ValueError, TypeError, KeyError):
            return self._envelope(request, 'error', 'TOOL_EXECUTION_STATE_ERROR')
        if 'envelope' in admitted:
            return _envelope_from_payload(admitted['envelope'])
        ticket = admitted['ticket']
        try:
            raw = await asyncio.wait_for(self._invoke(registration, dict(request.arguments)),
                                         timeout=admitted['timeout_seconds'])
        except asyncio.CancelledError:
            self._finish(ticket, failure='handler')
            raise
        except TimeoutError:
            return self._finish(ticket, failure='timeout')
        except Exception:
            return self._finish(ticket, failure='handler')
        try:
            return self._finish(ticket, raw=_public(raw))
        except (ValueError, TypeError, KeyError):
            return self._finish(ticket, failure='invalid')
        except RuntimeError:
            return self._envelope(request, 'error', 'TOOL_EXECUTION_STATE_ERROR')

    def resolve_cursor(self, cursor, *, actor_id):
        return self._call('cursor', cursor=str(cursor), actor_id=actor_id)

    async def _invoke(self, registration: ToolRegistration, arguments):
        if inspect.iscoroutinefunction(registration.handler):
            return await registration.handler(arguments)
        value = await asyncio.to_thread(registration.handler, arguments)
        return await value if inspect.isawaitable(value) else value
