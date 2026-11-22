"""A folder question projects requested fields from the located physical Excel row."""
import hashlib

from openpyxl import Workbook
import pytest

from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_folder_lookup_reads_original_fields_and_reports_partial_and_missing(native_turn, tmp_path):
    folder = tmp_path / 'schedules'
    folder.mkdir()
    book = Workbook()
    sheet = book.active
    sheet.title = 'Записи'
    sheet.append(['Наименование', 'Количество', 'Единица измерения', 'Масса'])
    sheet.append(['Кабель Альфа', 0, 'м', 500])
    sheet.append(['Кабель Бета', 17, None, 700])
    path = folder / 'Schedule.xlsx'
    book.save(path)
    t = native_turn
    dataset = t.client.post('/api/rag/datasets', json={'name':'Folder fields'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={
        'path':str(folder), 'dataset_id':dataset, 'background':True,
    }).raise_for_status().json()['intake_job_id']
    wait_for(lambda: t.client.get('/api/light/intake', params={'id':job}).json()['status']=='completed', timeout=60)
    for name, status, expected in [
        ('Кабель Альфа', 'verified', ['0', 'м']),
        ('Кабель Бета', 'partial', ['17', None]),
        ('Кабель Гамма', 'insufficient', []),
    ]:
        def choose_fields(body):
            assert 'Выбери из списка заголовки' in body['messages'][0]['content']
            return {'text':'{"fields":["Количество","Единица измерения"]}'}

        t.scripts.append(choose_fields)
        reply = t.client.post('/api/chat', json={
            'question':f'Какое количество и какая единица указаны для «{name}»?',
            'mode':'search', 'retrieval_mode':'hybrid', 'session_id':'lookup-'+name,
            'scope':{'scope_type':'datasets','dataset_ids':[dataset]},
        }).raise_for_status().json()
        receipt = reply['artifact']['table_facts']
        assert receipt['operation'] == 'lookup' and receipt['status'] == status
        assert len(reply['retrieval_trace']['contexts']) == 1
        if expected:
            row, = receipt['rows']
            assert [f['column'] for f in row['fields']] == [2, 3]
            actual = [f['value'] for f in row['fields']]
            assert float(actual[0]) == float(expected[0]) and actual[1] == expected[1]
            assert row['source_sha256'] == hashlib.sha256(path.read_bytes()).hexdigest()
            assert row['sheet'] == sheet.title and row['row'] == (2 if status == 'verified' else 3)
            assert reply['retrieval_trace']['citation_check']['linked'] == [row['source_index']]
        else:
            assert receipt['rows'] == [] and not reply['retrieval_trace']['citation_check']['linked']
