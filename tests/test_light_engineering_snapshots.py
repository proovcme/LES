"""Synthetic companion data through the actual native conversion and node pipeline."""
import json
import os

import pytest

from backend import native_core


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native conversion ABI required')


def snapshot():
    return {'schema': 'les.uvyazka-snapshot.v1', 'origin': {
        'project_id': 'example', 'dataset_id': 'immutable-snapshot',
        'source': {'filename': 'Example.rvt', 'sha256': 'a' * 64},
        'reader': {'engine': 'synthetic-contract', 'revision': '1'},
        'coverage': {'status': 'partial', 'limitations': ['Geometry not decoded']}},
        'object_count': 2, 'objects': [
            {'object_id': 'elements:42', 'native_id': '42', 'section': 'elements', 'name': 'Щит',
             'properties': {'power': {'value': 0, 'unit': 'kW'}, 'unknown': None, 'notes': 'Только щит. ' * 120}},
            {'object_id': 'elements:43', 'native_id': '43', 'section': 'elements', 'name': 'Кабель',
             'properties': {'length': {'value': 12.5, 'unit': 'm'}, 'notes': 'Только кабель. ' * 120}}]}


def convert(path, **settings):
    return native_core.required('document', request={
        'action': 'convert', 'schema': 'les.document-convert.v1', 'path': str(path.resolve()), **settings})


def test_original_ids_survive_real_chunking_preparation_and_parent_expansion(tmp_path):
    path = tmp_path / 'Projection.json'
    path.write_text(json.dumps(snapshot(), ensure_ascii=False), encoding='utf-8')
    raw = convert(path, pipeline='nodes', dataset_id='example', file_key='Projection.json',
                  data_dir=str(tmp_path), chunking={'unit': 'chars', 'chunk_size': 400,
                  'chunk_overlap': 25}, minimum=20, final_minimum=20)
    assert raw['status'] == 'completed'
    assert len(raw['nodes']) > 2
    prepared = native_core.required('document', request={'action': 'prepare_nodes', 'nodes': raw['nodes'],
        'dataset_id': 'example', 'file_name': 'Projection.json', 'revision': 'snapshot-revision',
        'window': 4, 'config': {'unit': 'chars', 'size': 400, 'minimum': 20}})['nodes']
    nodes = [n for n in prepared if n['payload'].get('node_role') != 'navigation']
    parents = {}
    for node in nodes:
        p = node['payload']
        target = p['model_locator']
        assert target['source_sha256'] == 'a' * 64
        assert target['source_name'] == 'Example.rvt'
        assert target['object_id'] == 'elements:' + target['native_id']
        parents.setdefault(p['parent_id'], set()).add(target['native_id'])
        assert not ('Только щит' in node['text'] and 'Только кабель' in node['text'])
        for key in ('context_before', 'context_after'):
            if target['native_id'] == '42':
                assert 'Только кабель' not in p.get(key, '')
            else:
                assert 'Только щит' not in p.get(key, '')
    assert all(len(ids) == 1 for ids in parents.values())
    assert {n['payload']['model_locator']['native_id'] for n in nodes} == {'42', '43'}


def test_invalid_count_is_not_indexed_as_ordinary_json(tmp_path):
    path = tmp_path / 'Broken.json'
    value = snapshot(); value['object_count'] = 3
    path.write_text(json.dumps(value), encoding='utf-8')
    with pytest.raises(native_core.NativeCoreError, match='ENGINEERING_SNAPSHOT_INCOMPLETE'):
        convert(path)
