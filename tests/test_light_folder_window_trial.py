"""The local comparison must use original bytes and may only publish a selected cell from its row."""
from copy import deepcopy
import json

import pytest

from tools.evaluate_folder_tables import pdf_cases
from tools.evaluate_folder_window import FIXTURE, evidence, input_for
from tools.folder_cell_reader import Row, resolve, resolve_literals, resolve_quote


def test_archives_keep_the_question_source_and_confounders_separate():
    present, removed = pdf_cases()
    assert present[3] == ('17.25', 'pcs', 1)
    assert removed[3] is None
    assert present[1] == removed[1]
    assert sum(isinstance(item, Row) for item in removed[2]) == 1
    data = json.loads((FIXTURE / 'present.json').read_bytes())
    excerpts = evidence(data, FIXTURE / 'present')
    body = data['captures'][0]['body']
    saved = deepcopy(body)
    compact = input_for('compact_with_tools', body, present[1], excerpts, set())
    assert compact['tools'] == body['tools'] and compact['options'] == body['options']
    text = compact['messages'][1]['content']
    positions = [text.index(item['text']) for item in excerpts]
    assert positions == sorted(positions) and text.endswith(present[1])
    assert body == saved
    data['captures'][0]['body']['messages'][0]['content'] += 'changed'
    with pytest.raises(ValueError, match='wire digest'):
        evidence(data, FIXTURE / 'present')


def test_cell_addresses_read_literal_zero_and_reject_another_rows_unit():
    rows = [Row(9, {'Q':'0', 'U':'pcs'}, {'Q':'Qty', 'U':'Unit'}),
            Row(2, {'Q':'7', 'U':'kg'}, {'Q':'Qty', 'U':'Unit'})]
    assert resolve('{"row":"S9","value":"Q","unit":"U"}', rows) == ('0', 'pcs', 9)
    assert resolve_quote('0 | pcs | 9', rows) == ('0', 'pcs', 9)
    assert resolve_quote('НЕТ', rows) is None
    with pytest.raises(ValueError):
        resolve_quote('0 | kg | 9', rows)
    with pytest.raises(ValueError):
        resolve_quote('999 | pcs | 9', rows)
    with pytest.raises(ValueError):
        resolve('{"row":"NONE","value":"Q","unit":"U"}', rows)
    with pytest.raises(ValueError):
        resolve_literals('[{"значение":"0","единица":"pcs","источник":true}]', rows)
    rows[0].cells['T'] = '0'
    with pytest.raises(ValueError, match='ambiguous'):
        resolve_quote('0 | pcs | 9', rows)
