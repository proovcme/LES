"""Request-local adapter to native budget/deadline/replay control."""
from backend import native_core


class HarnessLoop:
    def __init__(self, *, max_calls, deadline_seconds):
        self._state = native_core.required('harness', request=dict(
            action='new', max_calls=max_calls, deadline_seconds=deadline_seconds))['state']

    def _step(self, action, **arguments):
        result = native_core.required('harness', request=dict(
            action=action, state=self._state, **arguments))
        self._state = result['state']
        return result

    @property
    def remaining(self):
        return self._state['remaining']

    @property
    def round(self):
        return self._state['round']

    @property
    def reason(self):
        return self._state['reason']

    @property
    def stopped(self):
        return self._state['stopped']

    def enter_round(self):
        return self._step('round')['allowed']

    def select(self, identities):
        return self._step('select', identities=identities)['indexes']

    def begin(self, identity):
        return self._step('begin', identity=identity)['allowed']

    def finish(self, *, failed):
        self._step('finish', failed=failed)
