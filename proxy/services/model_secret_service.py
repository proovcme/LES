"""Typed facade for the allowlisted native model secret store."""
from __future__ import annotations
from dataclasses import dataclass,field
import os
from pathlib import Path
from typing import MutableMapping
from backend import native_core

class ModelSecretError(ValueError):
    """A secret reference or value violates the storage contract."""

@dataclass(frozen=True)
class SecretValue:
    _value: str = field(repr=False)

    def reveal(self) -> str:
        return self._value

@dataclass(frozen=True)
class SecretWriteReceipt:
    secret_ref: str
    status: str
    actor: str
    updated_at: str

def _native(action, **values):
    try:
        return native_core.required('model_secret', request={
            'schema': 'les.model-secret.v1', 'action': action, **values,
        })
    except native_core.NativeCoreError as error:
        raise ModelSecretError(str(error)) from error


def _secret_key(secret_ref: str) -> str:
    return _native('key', secret_ref=str(secret_ref or '').strip())['key']


class EnvironmentSecretStore:
    def __init__(self, env_path: str | Path, *, environ: MutableMapping[str, str] | None = None):
        self.env_path = Path(env_path)
        self.environ = os.environ if environ is None else environ

    def _call(self, action, secret_ref):
        key = _secret_key(secret_ref) if secret_ref is not None else None
        return _native(action, env_path=str(self.env_path.resolve()), secret_ref=secret_ref,
                       environment_value=str(self.environ.get(key) or '') if key else None)

    def status(self, secret_ref: str | None) -> str:
        return self._call('status', secret_ref)['status']

    def resolve(self, secret_ref: str | None) -> SecretValue | None:
        value = self._call('resolve', secret_ref)['value']
        return SecretValue(value) if value is not None else None

    def replace(self, secret_ref: str, value: str, *, actor: str) -> SecretWriteReceipt:
        key = _secret_key(secret_ref)
        receipt = _native('replace', env_path=str(self.env_path.resolve()), secret_ref=secret_ref,
                          value=str(value or ''), actor=str(actor or ''))
        self.environ[key] = str(value)
        return SecretWriteReceipt(**receipt)
