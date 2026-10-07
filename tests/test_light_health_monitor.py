import httpx
from backend.light_health_monitor import HealthMonitor


def test_health_threshold_recovery_and_instance_identity(monkeypatch):
    now = [0]
    responses = {'api': {'instance_id': 'own'}, 'ui': {'instance_id': 'own'}}
    keys = []
    class Client:
        def __init__(self, **kwargs): pass
        def __enter__(self): return self
        def __exit__(self, *args): pass
        def get(self, url, headers):
            if url.endswith('/collections'):
                keys.append(headers)
                return httpx.Response(200, json={})
            return httpx.Response(200, json=responses[url])
    monkeypatch.setattr(httpx, 'Client', Client)
    monitor = HealthMonitor(interval=5, failure_limit=3, clock=lambda: now[0])
    def probe():
        now[0] += 5
        return monitor.failed('api', 'ui', 'own', 'qdrant', 'own-key')
    assert not probe()
    responses['api'] = {'instance_id': 'other'}
    assert not probe()
    assert not probe()
    responses['api'] = {'instance_id': 'own'}
    assert not probe()  # Successful probe clears the count.
    responses['ui'] = []  # Invalid health payload cannot crash the launcher.
    assert not probe()
    assert not probe()
    assert probe()
    assert monitor.reason.endswith('UI')
    assert all(key == {'api-key': 'own-key'} for key in keys)
