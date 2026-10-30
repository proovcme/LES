"""Local, explicitly named trial model with the application's admission floor."""
from __future__ import annotations

from pathlib import Path
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path: sys.path.insert(0, str(ROOT))

import httpx
from tools.prepare_rag_trial_fixture import local_base
from proxy.services.model_resource_service import ModelResourceTarget
from proxy.services.runtime_admission import evaluate_chat_admission, live_memory_metrics


class TrialOllama:
    def __init__(self, url, model):
        self.http = httpx.Client(base_url=local_base(url), timeout=180, trust_env=False)
        try:
            models = {row['name'] for row in self.http.get('/api/tags').raise_for_status().json()['models']}
            if model not in models:
                raise ValueError('Explicit trial model must already be installed')
        except BaseException:
            self.http.close()
            raise
        self.model = model
        self.target = ModelResourceTarget(provider='ollama', model_id=model, revision_id='explicit-trial', locality='local')

    def close(self): self.http.close()

    def ask(self, messages, *, output_tokens=256, json_format=None):
        metrics = live_memory_metrics({})
        admission = evaluate_chat_admission(current_mode={'mode': 'chat'}, metrics_cache=metrics, connection=self.target)
        if not admission.allowed or not admission.memory['known']:
            raise RuntimeError('TRIAL_ADMISSION_BLOCKED: '+admission.reason)
        body = {'model': self.model, 'messages': messages, 'stream': False, 'think': False,
                'options': {'num_ctx': 8192, 'num_predict': output_tokens, 'temperature': 0}, 'keep_alive': '60s'}
        if json_format is not None: body['format'] = json_format
        started = time.perf_counter()
        response = self.http.post('/api/chat', json=body).raise_for_status().json()
        text = (response.get('message') or {}).get('content') or ''
        if not text.strip(): raise ValueError('TRIAL_EMPTY_ANSWER')
        return {'answer': text, 'milliseconds': (time.perf_counter()-started)*1000,
                'done_reason': response.get('done_reason'), 'eval_count': response.get('eval_count'),
                'prompt_eval_count': response.get('prompt_eval_count'), 'eval_duration': response.get('eval_duration'),
                'prompt_eval_duration': response.get('prompt_eval_duration'),
                'memory_before_gb': metrics.get('ram_free_gb')}
