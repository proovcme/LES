"""Compare the native folder chat and CitationQueryEngine on the same delivered evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import time

from tools import evaluate_part_reader_chat as trial
from tools.evaluate_llamaindex_folder import render


def cases(_):
    sensors = {
        'nord': 'Пост Кедр. Датчик Р-7 калибруют при 31 °C. Серийный номер К-093.',
        'ost': 'Пост Вяз. Датчик Р-7 калибруют при 19 °C. Серийный номер В-815.',
        'storage': 'Температура хранения запасного датчика Р-7 составляет 8 °C.',
    }
    yield 'sensors', sensors, {
        'complete': 'При каких температурах калибруют Р-7 на постах Кедр и Вяз?',
        'partial': 'Назови температуру калибровки и межповерочный интервал Р-7 на посту Кедр.',
        'absent': 'Каков срок гарантии датчика Р-7 на посту Вяз?',
    }
    yield 'editions', {
        'old': 'Порядок выдачи пропусков Скат. Редакция действовала с 1 января 2022 до 31 декабря 2024 включительно. Срок выдачи составлял 14 дней.',
        'current': 'Порядок выдачи пропусков Скат. С 1 января 2025 действует новая редакция. Срок выдачи составляет 6 дней.',
        'other': 'Порядок выдачи ключей Скат. Срок выдачи ключей составляет 2 дня.',
    }, {
        'historical': 'За сколько дней выдавали пропуск Скат на 15 июня 2024?',
        'current': 'На 1 февраля 2025 сколько дней отведено на выдачу пропуска Скат?',
        'rephrased': 'Сколько ждать именно пропуск Скат в феврале 2025 года?',
    }


def compare(t, dataset, case, question, documents, llm, server, models):
    session = 'transfer-' + dataset + '-' + case
    calls = []

    def native_response(body):
        with sqlite3.connect(t.database) as db:
            trace, = db.execute('SELECT retrieval_trace_json FROM chat_history WHERE session_id=? ORDER BY id DESC LIMIT 1',
                                (session,)).fetchone()
        sources = json.loads(trace)['source_map']
        # Use actual message order. CitationQueryEngine assigns consecutive numbers.
        text = '\n'.join(m['content'] for m in body['messages'])
        ordered = []
        for source in sources:
            key = Path(source['doc_name']).stem
            passage = documents[key]
            if passage not in text or hashlib.sha256(passage.encode()).hexdigest() != source['source_sha256']:
                raise ValueError('Excerpt absent from actual request or different original')
            ordered.append((text.index(passage), key, source['index']))
        ordered.sort()
        wire = {k: body.get(k) for k in ('model', 'temperature', 'reasoning_effort')}
        wire.update(visible=[key for _, key, _ in ordered], source_numbers=[index for _, _, index in ordered],
                    max_tokens=body.get('max_tokens',body.get('max_completion_tokens')))
        calls.append(wire)
        result = llm.post('chat/completions', json={**body, 'stream': False}).raise_for_status().json()
        choice = result['choices'][0]
        message = choice['message']
        actions = [{'tool': c['function']['name'], 'args': json.loads(c['function']['arguments'])}
                   for c in message.get('tool_calls') or []]
        return {'text': message.get('content') or '', 'finish': choice['finish_reason'], 'calls': actions}

    t.scripts[:] = [native_response] * 16
    start = time.monotonic()
    reply = trial.post(t.client, '/api/chat', dict(question=question, session_id=session, mode='search',
                       retrieval_mode='hybrid', scope=dict(scope_type='datasets', dataset_ids=[dataset])), server, models)
    data = reply.json()
    row = dict(case=case, question=question, http=reply.status_code, error=data.get('code'),
               answer=data.get('answer'), calls=calls, seconds=round(time.monotonic()-start,2))
    sources = data.get('source_map', [])
    row['sources'] = {s['index']: Path(s['doc_name']).stem for s in sources}
    for source in sources:
        raw = t.client.get('/api/documents/by-id/' + source['doc_id'] + '/raw').raise_for_status().content
        if hashlib.sha256(raw).hexdigest() != source['source_sha256']:
            raise ValueError('Original source digest changed')
    if calls and calls[0]['visible']:
        for mode in ('citations', 'les-citations'):
            row[mode] = render(dict(case=case, variant='first delivered window', question=question, calls=calls),
                               documents, llm, mode)
    # The shared folder runner reports verified original names with each completed question.
    row['linked_originals'] = {str(s['index']):s['doc_name'] for s in sources
                               if s['index'] in (data.get('retrieval_trace',{}).get('citation_check') or {}).get('linked',[])}
    return row


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--model',required=True)
    parser.add_argument('--ollama',required=True)
    parser.add_argument('--scratch',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args = parser.parse_args()
    args.fixtures = None
    trial.run(args, folder_cases=cases, query=compare)
