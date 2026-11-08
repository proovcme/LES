"""Typed compatibility facade; model registry storage and transactions live in Rust."""
from __future__ import annotations
from datetime import datetime
import json
from pathlib import Path
from backend.rag_config import rag_meta_db_path
from backend.workspace_native import catalog_call
from proxy.services.model_connection_contracts import (
    CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState,
    ConnectionLocality, ConnectionRole, ModelConnectionRevision, RoleBinding,
)

class ModelConnectionRegistryError(RuntimeError):
    """Stable fail-closed registry error."""

class RevisionConflictError(ModelConnectionRegistryError):
    """Compare-and-swap failed because another revision won."""

class ModelConnectionRegistry:
    def __init__(self, db_path: str | Path | None = None):
        self.db_path = Path(db_path) if db_path is not None else Path(rag_meta_db_path())
        self._call('init')

    def _call(self, action, **arguments):
        return catalog_call(self.db_path, 'model_registry_' + action, **arguments)

    @staticmethod
    def _revision_from_row(row) -> ModelConnectionRevision:
        return ModelConnectionRevision(
            connection_id=row["connection_id"],
            revision_id=row["revision_id"],
            revision_no=int(row["revision_no"]),
            display_name=row["display_name"],
            protocol=row["protocol"],
            base_url=row["base_url"],
            model_id=row["model_id"],
            locality=ConnectionLocality(row["locality"]),
            requested_context_tokens=row["requested_context_tokens"],
            secret_ref=row["secret_ref"],
            extension_type=row["extension_type"],
            enabled=bool(row["enabled"]),
            created_at=row["created_at"],
            created_by=row["created_by"],
        )

    @staticmethod
    def _binding_from_row(row) -> RoleBinding:
        return RoleBinding(
            role=ConnectionRole(row["role"]),
            binding_revision=int(row["binding_revision"]),
            connection_revision_id=row["connection_revision_id"],
            bound_at=row["bound_at"],
            bound_by=row["bound_by"],
        )

    def create_connection(
        self,
        *,
        display_name: str,
        base_url: str,
        model_id: str,
        locality: ConnectionLocality,
        requested_context_tokens: int | None,
        secret_ref: str | None,
        extension_type: str | None,
        actor: str,
    ) -> ModelConnectionRevision:
        return self._revision_from_row(self._call(
            'create', display_name=display_name, base_url=base_url, model_id=model_id,
            locality=locality, requested_context_tokens=requested_context_tokens,
            secret_ref=secret_ref, extension_type=extension_type, actor=actor,
        ))

    def import_connection(
        self,
        *,
        stable_connection_id: str,
        display_name: str,
        base_url: str,
        model_id: str,
        locality: ConnectionLocality,
        requested_context_tokens: int | None,
        secret_ref: str | None,
        extension_type: str | None,
        actor: str,
    ) -> ModelConnectionRevision:
        return self._revision_from_row(self._call(
            'import', stable_connection_id=stable_connection_id,
            display_name=display_name, base_url=base_url, model_id=model_id,
            locality=locality, requested_context_tokens=requested_context_tokens,
            secret_ref=secret_ref, extension_type=extension_type, actor=actor,
        ))

    def revise_connection(
        self,
        connection_id: str,
        *,
        expected_revision_id: str,
        actor: str,
        **changes: object,
    ) -> ModelConnectionRevision:
        return self._revision_from_row(self._call('revise', connection_id=connection_id, expected_revision_id=expected_revision_id, actor=actor, changes=changes))

    def disable_connection(
        self,
        connection_id: str,
        *,
        expected_revision_id: str,
        actor: str,
    ) -> ModelConnectionRevision:
        return self.revise_connection(connection_id, expected_revision_id=expected_revision_id, actor=actor, enabled=False)

    def get_connection(self, connection_id: str) -> ModelConnectionRevision:
        return self._revision_from_row(self._call('get', connection_id=connection_id))

    def get_revision(self, revision_id: str) -> ModelConnectionRevision:
        return self._revision_from_row(self._call('revision', revision_id=revision_id))

    def list_connections(
        self,
        *,
        include_disabled: bool = False,
    ) -> tuple[ModelConnectionRevision, ...]:
        return tuple(self._revision_from_row(row) for row in self._call('list', include_disabled=include_disabled))

    def list_revisions(self, connection_id: str) -> tuple[ModelConnectionRevision, ...]:
        return tuple(self._revision_from_row(row) for row in self._call('revisions', connection_id=connection_id))

    def bind_role(
        self,
        role: ConnectionRole,
        connection_revision_id: str,
        *,
        expected_binding_revision: int | None,
        actor: str,
    ) -> RoleBinding:
        return self._binding_from_row(self._call('bind', role=ConnectionRole(role).value, connection_revision_id=connection_revision_id, expected_binding_revision=expected_binding_revision, actor=actor))

    def get_role_binding(self, role: ConnectionRole) -> RoleBinding | None:
        row = self._call('binding', role=ConnectionRole(role).value)
        return self._binding_from_row(row) if row is not None else None

    def save_capability_snapshot(
        self,
        snapshot: CapabilitySnapshot,
        *,
        actor: str,
    ) -> None:
        self._call('save_snapshot', actor=actor, snapshot={
            'snapshot_id': snapshot.snapshot_id,
            'connection_revision_id': snapshot.connection_revision_id,
            'observed_at': snapshot.observed_at.isoformat(),
            'expires_at': snapshot.expires_at.isoformat(),
            'transport_options': dict(snapshot.transport_options),
            'observations': [{
                'capability': item.capability.value, 'state': item.state.value,
                'evidence_source': item.evidence_source,
                'observed_at': item.observed_at.isoformat(), 'detail': item.detail,
            } for item in snapshot.observations],
        })

    def latest_capability_snapshot(
        self,
        connection_revision_id: str,
    ) -> CapabilitySnapshot | None:
        row = self._call('snapshot', connection_revision_id=connection_revision_id)
        return self._snapshot_from_row(row) if row is not None else None

    def execution_snapshot(self, **selector):
        """Resolve the role, head and capability rows at one database revision."""
        result = catalog_call(self.db_path, 'model_execution_snapshot', **selector)
        if result is None:
            if "revision_id" in selector:
                raise ModelConnectionRegistryError("CONNECTION_REVISION_NOT_FOUND")
            return None
        if "missing" in result:
            raise ModelConnectionRegistryError(result["missing"])
        return (
            self._revision_from_row(result["revision"]),
            result["enabled"],
            self._snapshot_from_row(result["snapshot"]) if result["snapshot"] is not None else None,
        )

    @staticmethod
    def _snapshot_from_row(row) -> CapabilitySnapshot:
        observations = tuple(
            CapabilityObservation(
                capability=CapabilityName(item["capability"]),
                state=CapabilityState(item["state"]),
                evidence_source=item["evidence_source"],
                observed_at=datetime.fromisoformat(item["observed_at"]),
                detail=item.get("detail", ""),
            )
            for item in json.loads(row["observations_json"])
        )
        return CapabilitySnapshot(
            snapshot_id=row["snapshot_id"],
            connection_revision_id=row["connection_revision_id"],
            observations=observations,
            observed_at=datetime.fromisoformat(row["observed_at"]),
            expires_at=datetime.fromisoformat(row["expires_at"]),
            transport_options=json.loads(row["transport_options_json"]),
        )
