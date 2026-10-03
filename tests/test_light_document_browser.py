"""Late API replies must never replace the source currently selected by the user."""
import asyncio
from types import SimpleNamespace

import pytest

from sovushka.components import document_browser as module


def browser():
    state = dict(selected_dataset='a', datasets=[{'id': 'a', 'name': 'First'}],
                 documents=[], query='question', document_filter='', hits=[{'text': 'old search'}])
    controller = module.DocumentBrowser(state, {}, surface='data', can_manage=False,
                                        initial_mode='map', initial_note='', initial_dataset='a')
    controller.view = SimpleNamespace(**{
        name: lambda: None for name in ('_render_documents', '_render_view', '_render_all',
                                        '_render_datasets', '_render_readiness_summary', '_render_status_error')})
    return controller


@pytest.mark.asyncio
async def test_old_file_response_cannot_replace_new_file_or_fetch_unused_pdf_preview(monkeypatch):
    current = browser()
    pending = {}

    async def get(route):
        assert '/chunks?' in route  # No hidden OCR/preview work for a text-only reader.
        pending[route] = asyncio.get_running_loop().create_future()
        return await pending[route]

    monkeypatch.setattr(module, 'api_get', get)
    old = asyncio.create_task(current._inspect_composition_file('old', 'Старый.pdf'))
    await asyncio.sleep(0)
    new = asyncio.create_task(current._inspect_composition_file('new', 'New #1.pdf'))
    await asyncio.sleep(0)
    next(v for k, v in pending.items() if '/new/' in k).set_result({'chunks': [{'text': 'new content'}], 'total': 1})
    await new
    next(v for k, v in pending.items() if '/old/' in k).set_result({'chunks': [{'text': 'old content'}], 'total': 1})
    await old
    assert current.state['composition_file']['doc_id'] == 'new'
    assert current.state['composition_file']['chunks'] == [{'text': 'new content'}]
    assert current.state['hits'] == []


@pytest.mark.asyncio
@pytest.mark.parametrize('operation,key', [
    ('_load_documents', 'documents'), ('_load_memory', 'dataset_memory'),
    ('_load_pdf_extract_summary', 'pdf_extract'), ('_load_rag_readiness', 'rag_readiness'),
])
async def test_old_dataset_reply_is_ignored_even_after_switching_back(monkeypatch, operation, key):
    current = browser()
    pending = asyncio.get_running_loop().create_future()

    async def get(_route):
        return await pending

    monkeypatch.setattr(module, 'api_get', get)
    task = asyncio.create_task(getattr(current, operation)())
    await asyncio.sleep(0)
    # a -> b -> a; comparing only the dataset ID would wrongly accept this reply.
    current._selection_revision += 2
    current.state[key] = {'current': True}
    pending.set_result({'documents': [{'id': 'old'}], 'old': True})
    await task
    assert current.state[key] == {'current': True}


@pytest.mark.asyncio
async def test_search_reply_cannot_hide_newly_opened_original(monkeypatch):
    current = browser()
    pending = asyncio.get_running_loop().create_future()

    async def get(route):
        if '/search?' in route:
            return await pending
        return {'chunks': [{'text': 'selected document'}]}

    monkeypatch.setattr(module, 'api_get', get)
    old = asyncio.create_task(current._search('dataset'))
    await asyncio.sleep(0)
    await current._inspect_composition_file('new', 'New.txt')
    pending.set_result({'hits': [{'text': 'late search'}]})
    await old
    assert current.state['composition_file']['doc_id'] == 'new'
    assert current.state['hits'] == []
