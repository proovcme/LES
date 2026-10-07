"""Execute only operator-reviewed, hash-pinned pure skill functions."""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from uuid import uuid4

from backend.runtime_paths import mutable_path
from proxy.services.installed_skill_service import _file, library_root


def approved_scripts():
    path = library_root() / 'approved-compute.json'
    return json.loads(path.read_text(encoding='utf-8')) if path.exists() else []


def calculate(skill, path, items):
    approved = next((item for item in approved_scripts() if item['skill'] == skill and item['path'] == path), None)
    if approved is None:
        raise ValueError('Скрипт не разрешён для вычислений')
    source = _file(skill, path).read_bytes()
    digest = hashlib.sha256(source).hexdigest()
    if digest != approved['sha256']:
        raise ValueError('Скрипт изменён после проверки. Выполнение остановлено')
    if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*', approved['entrypoint']):
        raise ValueError('Некорректная точка входа')
    if not isinstance(items, list) or not 1 <= len(items) <= 100:
        raise ValueError('Передайте от 1 до 100 позиций')
    ids = [item.get('id') for item in items if isinstance(item, dict)]
    if len(ids) != len(items) or any(not isinstance(key, str) or not 1 <= len(key) <= 100 for key in ids) or len(set(ids)) != len(ids):
        raise ValueError('Для каждой позиции нужен уникальный строковый ID')
    if any(not isinstance(item.get('input'), dict) for item in items):
        raise ValueError('Входные данные каждой позиции должны быть объектом JSON')
    request = json.dumps({'sha256': digest, 'entrypoint': approved['entrypoint'], 'items': items},
                         ensure_ascii=False, allow_nan=False)
    if len(request.encode('utf-8')) > 256000:
        raise ValueError('Входные данные превышают 256 КБ')
    job_id = uuid4().hex
    work = mutable_path('artifacts/skill-compute') / job_id
    work.mkdir(parents=True)
    (work / 'script.py').write_bytes(source)
    (work / 'input.json').write_text(request, encoding='utf-8')
    worker = Path(__file__).resolve().parents[2] / 'tools/skill_compute_worker.py'
    # No connection tokens, model settings or user-defined Python paths in the child.
    environment = {key: value for key, value in os.environ.items()
                   if key.upper() in {'SYSTEMROOT', 'WINDIR', 'TEMP', 'TMP'}}
    interrupted = False
    try:
        completed = subprocess.run([sys.executable, '-I', '-S', '-X', 'utf8', str(worker)],
            cwd=work, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, timeout=20,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        interrupted = completed.returncode != 0
    except subprocess.TimeoutExpired:
        interrupted = True
    results = [json.loads(file.read_text(encoding='utf-8')) for file in sorted(work.glob('[0-9][0-9][0-9][0-9].json'))]
    summary = {'job_id': job_id, 'script_sha256': digest, 'total': len(items),
               'completed': len(results), 'succeeded': sum(row['status'] == 'ok' for row in results),
               'interrupted': interrupted, 'pending_ids': ids[len(results):]}
    (work / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False), encoding='utf-8')
    return {**summary, 'rows': results[:10], 'next_offset': 10 if len(results) > 10 else None}


def read_job(job_id, offset=0):
    if not re.fullmatch(r'[a-f0-9]{32}', job_id):
        raise ValueError('Некорректный номер расчёта')
    work = mutable_path('artifacts/skill-compute') / job_id
    if not work.is_dir():
        raise ValueError('Расчёт не найден')
    files = sorted(work.glob('[0-9][0-9][0-9][0-9].json'))
    offset = max(0, int(offset))
    rows = [json.loads(file.read_text(encoding='utf-8')) for file in files[offset:offset + 10]]
    return {'job_id': job_id, 'rows': rows, 'saved_rows': len(files),
            'next_offset': offset + len(rows) if offset + len(rows) < len(files) else None}


def register_tools(registry):
    from proxy.services.tool_contract_service import (
        ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
    from proxy.services.tool_registry_service import ToolRegistration
    definitions = [
        ('run_skill_calculation', 'Вычисление по навыку',
         'Run an approved pure Python skill function on JSON inputs with unique row IDs. '
         'Persist each row, return job_id and pending_ids; this does not verify input facts. '
         'Only explicitly approved script hashes are executable. List approvals using read_skill_calculation.',
         {'skill': {'type': 'string'}, 'path': {'type': 'string'},
          'items': {'type': 'array', 'minItems': 1, 'maxItems': 100, 'items': {'type': 'object'}}},
         ['skill', 'path', 'items'], calculate, EffectClass.COMPUTE),
        ('read_skill_calculation', 'Результаты вычисления',
         'Without job_id list approved Python skill scripts and entrypoints. With job_id read saved rows; '
         'continue using next_offset until null.',
         {'job_id': {'type': 'string'}, 'offset': {'type': 'integer', 'minimum': 0}}, [],
         lambda **args: read_job(**args) if args.get('job_id') else {'scripts': approved_scripts()}, EffectClass.READ),
    ]
    for name, title, description, properties, required, action, effect in definitions:
        async def handler(arguments, action=action, name=name):
            from proxy.services.tool_harness_service import _result
            result = await asyncio.to_thread(action, **arguments)
            return _result(tool=name, operation='skill_compute', inputs=[],
                           status='error' if result.get('interrupted') else 'ok',
                           result=result, trace='approved_skill_compute')
        registry.register(ToolRegistration(contract=ToolContract(
            name=name, version='1.0.0', title=title, category='skills', summary=description,
            input_schema={'type': 'object', 'properties': properties, 'required': required,
                          'additionalProperties': False}, result_schema='les_tool_result_v1',
            effect=effect, scopes=('skills',), timeout_seconds=25,
            retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=32000, max_items=16), model_owned_fields=(),
            provenance='Operator-approved pure calculation; inputs and outputs saved', tags=('skills',)), handler=handler))
