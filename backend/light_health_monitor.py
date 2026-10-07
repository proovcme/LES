"""Bounded liveness probes, independent of model latency and generation progress."""
import time
import httpx


class HealthMonitor:
    def __init__(self, *, interval=5.0, failure_limit=6, clock=time.monotonic):
        self.interval, self.failure_limit, self.clock = interval, failure_limit, clock
        self.next_probe = 0.0
        self.failures = {}
        self.reason = ''

    def failed(self, api_url, ui_url, instance_id, qdrant_url, qdrant_key):
        now = self.clock()
        if now < self.next_probe:
            return False
        checks = [('API', api_url, {}), ('UI', ui_url, {}),
                  ('Qdrant', qdrant_url + '/collections', {'api-key': qdrant_key})]
        with httpx.Client(timeout=2, trust_env=False) as client:
            for name, url, headers in checks:
                try:
                    response = client.get(url, headers=headers)
                    payload = response.json() if response.status_code == 200 and name != 'Qdrant' else {}
                    healthy = response.status_code == 200 and (
                        name == 'Qdrant' or isinstance(payload, dict) and payload.get('instance_id') == instance_id)
                except (httpx.HTTPError, ValueError):
                    healthy = False
                self.failures[name] = 0 if healthy else self.failures.get(name, 0) + 1
        self.next_probe = self.clock() + self.interval
        stalled = [name for name, count in self.failures.items() if count >= self.failure_limit]
        self.reason = 'Не отвечает служба: ' + ', '.join(stalled) if stalled else ''
        return bool(stalled)
