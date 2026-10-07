import hashlib
import json

import pytest
from proxy.services import installed_skill_service as skills
from proxy.services import skill_compute_service as compute


@pytest.fixture
def installed(tmp_path, monkeypatch):
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT', str(tmp_path))
    library = tmp_path / 'storage/skills'
    package = library / 'arithmetic'
    package.mkdir(parents=True)
    (package / 'SKILL.md').write_text('description: Arithmetic', encoding='utf-8')
    script = package / 'calculator.py'
    script.write_text('from decimal import Decimal\n'
                      'def calculate(value):\n'
                      '    return {"cost": str(Decimal(value["q"]) * Decimal(value["price"]))}\n', encoding='utf-8')
    approval = {'skill': 'arithmetic', 'path': 'calculator.py', 'entrypoint': 'calculate',
                'sha256': hashlib.sha256(script.read_bytes()).hexdigest()}
    (library / 'approved-compute.json').write_text(json.dumps([approval]), encoding='utf-8')
    return script


def test_real_worker_calculates_and_persists_70_rows(installed):
    items = [{'id': f'row-{i:03d}', 'input': {'q': str(i), 'price': '10.25'}} for i in range(1, 71)]
    result = compute.calculate('arithmetic', 'calculator.py', items)
    assert result['completed'] == result['succeeded'] == 70
    assert not result['interrupted'] and not result['pending_ids']
    rows, offset = [], 0
    while offset is not None:
        page = compute.read_job(result['job_id'], offset)
        rows.extend(page['rows'])
        offset = page['next_offset']
    assert [row['id'] for row in rows] == [item['id'] for item in items]
    assert rows[-1]['result']['cost'] == '717.50'


def test_modified_script_and_unapproved_path_never_execute(installed):
    installed.write_text('raise RuntimeError("must not execute")', encoding='utf-8')
    with pytest.raises(ValueError, match='изменён'):
        compute.calculate('arithmetic', 'calculator.py', [{'id': '1', 'input': {}}])
    with pytest.raises(ValueError, match='не разрешён'):
        compute.calculate('arithmetic', '../elsewhere.py', [{'id': '1', 'input': {}}])


def test_bad_row_does_not_hide_remaining_rows(installed):
    result = compute.calculate('arithmetic', 'calculator.py', [
        {'id': 'bad', 'input': {}}, {'id': 'good', 'input': {'q': '2', 'price': '3'}}])
    assert result['completed'] == 2 and result['succeeded'] == 1
    assert result['rows'][0]['status'] == 'error'
    assert result['rows'][1]['result']['cost'] == '6'
    with pytest.raises(ValueError, match='уникальный'):
        compute.calculate('arithmetic', 'calculator.py', [{'id': 'same', 'input': {}}] * 2)
