"""Resource identity follows explicit immutable role revisions, never environment presets."""
from dataclasses import dataclass
import logging

from proxy.services.model_connection_contracts import ConnectionRole

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class ModelResourceTarget:
    provider: str = 'unassigned'
    model_id: str = ''
    revision_id: str = ''
    locality: str = 'unknown'

    @property
    def remote(self) -> bool:
        # Unknown identity must keep local protection.
        return self.locality == 'remote'


def resource_target(connection) -> ModelResourceTarget:
    if isinstance(connection, ModelResourceTarget):
        return connection
    locality = getattr(connection, 'locality', None)
    return ModelResourceTarget(
        provider=getattr(connection, 'extension_type', None) or 'openai-compatible',
        model_id=getattr(connection, 'model_id', ''),
        revision_id=getattr(connection, 'revision_id', ''),
        locality=getattr(locality, 'value', locality) or 'unknown',
    )


def assigned_resource_target(role=ConnectionRole.ANSWER) -> ModelResourceTarget:
    """Read role identity without loading models, resolving DNS or reading credentials."""
    from proxy.services.model_connection_registry_service import ModelConnectionRegistry
    try:
        registry = ModelConnectionRegistry()
        binding = registry.get_role_binding(role)
        if binding is None:
            return ModelResourceTarget()
        revision = registry.get_revision(binding.connection_revision_id)
        if not registry.get_connection(revision.connection_id).enabled:
            return ModelResourceTarget(provider='disabled')
        return resource_target(revision)
    except Exception:
        logger.exception('Unable to read model resource identity')
        return ModelResourceTarget(provider='unavailable')


def local_ollama_native_url(connection, operation):
    """Only authenticated-free literal loopback Ollama supports local residency IO."""
    import ipaddress
    from urllib.parse import urlsplit, urlunsplit
    if operation not in {'generate', 'embed'}:
        raise ValueError('Unsupported residency operation')
    if (getattr(connection, 'extension_type', None) != 'ollama'
            or getattr(connection, 'secret_ref', None)):
        return None
    locality = getattr(connection, 'locality', None)
    if getattr(locality, 'value', locality) != 'loopback':
        return None
    endpoint = getattr(connection, 'endpoint', None)
    url = urlsplit(getattr(endpoint, 'canonical_base_url', connection.base_url))
    if (url.scheme not in {'http', 'https'} or url.username or url.password
            or url.query or url.fragment):
        return None
    try:
        loopback = ipaddress.ip_address(url.hostname).is_loopback
    except ValueError:
        loopback = url.hostname == 'localhost'
    if not loopback:
        return None
    path = url.path.rstrip('/')
    for suffix in ('/api/v1', '/v1'):
        if path.endswith(suffix):
            path = path[:-len(suffix)]
            break
    return urlunsplit((url.scheme, url.netloc, path + '/api/' + operation, '', ''))
