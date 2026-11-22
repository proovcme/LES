"""Replay a captured native Ollama folder window locally on the same model.

The two captures come from the public chat with and without the fact in the
physical document. Only the incident's synthetic PDF and TXT are supported.
"""
import argparse
from hashlib import sha256
import json
from pathlib import Path
import time
from urllib.request import ProxyHandler, Request, build_opener

FIXTURE = Path(__file__).resolve().parents[1] / 'docs/acceptance/folder-window-935'


def evidence(capture, folder):
    trace = capture['reply']['retrieval_trace']
    ctx = trace['contexts'][0]
    wire = capture['captures'][0]
    if wire['path'] != '/api/chat' or ctx['delivery'] != 'confirmed':
        raise ValueError('Expected a delivered native Ollama chat')
    serialized = json.dumps(wire['body'], ensure_ascii=False, separators=(',', ':')).encode()
    if sha256(serialized).hexdigest() != wire['sha256']:
        raise ValueError('Saved provider request differs from its wire digest')
    packed = '\n'.join(m['content'] for m in wire['body']['messages'])
    sources = {s['index']: s for s in ctx['sources']}
    items = []
    positions = []
    for section in ctx['context']['sections']:
        if section['kind'] != 'evidence':
            continue
        for obj in section['objects']:
            positions.append(packed.index(obj['text']))
            for block in obj['payload'].get('blocks', [obj['payload']]):
                s = sources[block['source_index']]
                filename = Path(s['doc_name']).name
                original = (folder / filename).read_bytes()
                if sha256(original).hexdigest() != s['source_sha256']:
                    raise ValueError('Original changed')
                _, text = block['text'].split('\n', 1)
                digest = sha256(text.encode()).hexdigest()
                if digest != (s.get('reading_unit_sha256') or s['source_sha256']):
                    raise ValueError('Excerpt changed')
                items.append(dict(id=s['index'], text=text, file=filename, page=s.get('page'),
                                  line=s.get('pdf_excerpt_line'), sha256=digest))
    if positions != sorted(set(positions)) or [s['id'] for s in items] != list(sources):
        raise ValueError('Source order differs from the wire')
    return items


READ = ('Ответь на вопрос по выдержкам. Укажи источник в формате [Источник N]. '
        'Если в выдержках нет запрошенных сведений, ответь [[NO_EVIDENCE]].')
PICK = ('Найди строку, которая отвечает на вопрос. Верни JSON с целыми числами '
        '{"source": номер источника строки, "value": номер столбца значения, '
        '"unit": номер столбца единицы}. Столбцы нумеруются с 1 слева направо. '
        'Если подходящей строки нет, верни null.')


def cell_answer(raw, cells):
    selection = json.loads(raw)
    if selection is None:
        return '[[NO_EVIDENCE]]'
    if (not isinstance(selection, dict) or set(selection) != {'source', 'value', 'unit'}
            or any(type(n) is not int or n < 1 for n in selection.values())):
        raise ValueError('Invalid selection')
    source, value, unit = (selection[k] for k in ('source', 'value', 'unit'))
    if source not in cells or value == unit or max(value, unit) > len(cells[source]):
        raise ValueError('Cell outside a delivered data row')
    row = cells[source]
    if not row[value-1].strip() or not row[unit-1].strip():
        raise ValueError('Empty cell')
    return f'{row[value-1]} {row[unit-1]} [Источник {source}]'


def input_for(arm, body, question, items, oracle_ids):
    if arm == 'native':
        return dict(body)
    if arm == 'no_tools':
        return {key: value for key, value in body.items() if key != 'tools'}
    if arm in ('question_only', 'plain_evidence'):
        messages = [dict(m) for m in body['messages']]
        if [m['role'] for m in messages] != ['system','user','user'] or question not in messages[2]['content']:
            raise ValueError('Unexpected shape of the recorded first turn')
        if arm == 'question_only':
            messages[2]['content'] = question
        else:
            records = []
            for item in items:
                page = f" · стр. {item['page']}" if item['page'] else ''
                records.append(f"[Источник {item['id']} | {item['file']}{page}]\n{item['text']}")
            messages[1]['content'] = '\n'.join(records)
        return dict(body,messages=messages)
    visible = [s for s in items if arm != 'row' or s['id'] in oracle_ids]
    excerpts = '\n'.join(f"[Источник {s['id']} | {s['file']}] {s['text']}" for s in visible)
    prompt = PICK if arm == 'cells' else READ
    req = dict(body, messages=[dict(role='system', content=prompt),
                               dict(role='user', content=excerpts+'\n\n'+question)])
    if arm != 'compact_with_tools':
        req.pop('tools', None)
    return req


def generate(body):
    # The experiment never sends excerpts to a configurable/remote host.
    req = Request('http://127.0.0.1:11434/api/chat', data=json.dumps(body).encode(),
                  headers={'Content-Type': 'application/json'})
    text, calls, end = [], [], {}
    with build_opener(ProxyHandler({})).open(req, timeout=240) as reply:
        for line in reply:
            event = json.loads(line)
            if event.get('error'):
                raise RuntimeError(event['error'])
            msg = event.get('message', {})
            text.append(msg.get('content', ''))
            calls.extend(msg.get('tool_calls') or [])
            if event.get('done'):
                end = event
    if not end:
        raise IOError('Incomplete Ollama stream')
    return ''.join(text), calls, end


def run_prose(args):
    matrix = json.loads((FIXTURE.parent / 'folder-transfer-935.json').read_bytes())
    body = json.loads(args.present.read_bytes())['captures'][0]['body']
    with args.output.open('x', encoding='utf8') as out:
        for seed in args.seeds:
            for case in matrix['runs'][-1]['questions']:
                ids = case['first_window']
                corpus = matrix['corpora'][case['dataset']]
                items = [dict(id=i, file=key+'.txt', text=corpus[key]) for i, key in enumerate(ids, 1)]
                request = input_for('compact', body, case['question'], items, set())
                request['options'] = {**body['options'], 'seed':seed}
                raw, calls, end = generate(request)
                result = dict(case=case['case'], seed=seed, ids=ids, answer=raw,
                              tool_calls=len(calls), finished=end.get('done_reason'))
                print(json.dumps(result, ensure_ascii=False), file=out, flush=True)


def main(args):
    if args.prose:
        return run_prose(args)
    archive = json.loads(args.baseline.read_bytes())
    conditions = {}
    for cond, path, folder in [('present', args.present, args.original_folder),
                               ('removed', args.removed, args.removed_folder)]:
        capture = json.loads(path.read_bytes())
        wire = capture['captures'][0]
        items = evidence(capture, folder)
        question = archive['question']
        body = wire['body']
        assert question in '\n'.join(m['content'] for m in body['messages'])
        assert body['model'] == archive['answer_model'] and body['stream'] is True
        target = [s for s in items if s['text'] == archive['expected_row']]
        headers = [s for s in items if s['text'] == '|Position|Item|Unit|Quantity|']
        assert len(headers) == 1 and len(target) == int(cond == 'present')
        if cond == 'present':
            assert target[0]['sha256'] == archive['confirmed_first_request']['reading_unit_sha256']
        else:
            assert b'17.25' not in (folder/'Register.pdf').read_bytes()
            assert '17.25' not in json.dumps(body, ensure_ascii=False)
        oracle_ids = {s['id'] for s in headers + target}
        cells = {s['id']: s['text'][1:-1].split('|') for s in items
                 if s['line'] and s['id'] != headers[0]['id'] and s['text'].startswith('|') and s['text'].endswith('|')}
        conditions[cond] = body, items, oracle_ids, cells, wire['sha256']

    with args.output.open('x', encoding='utf8') as out:
        for pos, seed in enumerate(args.seeds):
            arms = args.arms
            for arm in arms[pos % len(arms):] + arms[:pos % len(arms)]:
                for cond in (('present','removed') if pos%2==0 else ('removed','present')):
                    body, items, oracle_ids, cells, wire_hash = conditions[cond]
                    request = input_for(arm, body, question, items, oracle_ids)
                    request['options'] = {**body['options'], 'seed': seed}
                    start = time.monotonic()
                    raw, calls, end = generate(request)
                    answer, error = raw, None
                    if arm == 'cells':
                        try:
                            answer = cell_answer(raw, cells)
                        except ValueError as exc:
                            answer, error = None, str(exc)
                    row = dict(condition=cond, arm=arm, seed=seed, first_wire_sha256=wire_hash,
                               raw=raw, answer=answer, error=error, tool_calls=len(calls),
                               done=end.get('done_reason'), prompt_tokens=end.get('prompt_eval_count'),
                               seconds=round(time.monotonic()-start,2))
                    print(json.dumps(row,ensure_ascii=False), file=out, flush=True)
                    print(cond, arm, seed, row['prompt_tokens'], row['seconds'], flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for key, name in [('present','present.json'), ('removed','removed.json'),
                      ('original-folder','present'), ('removed-folder','removed')]:
        parser.add_argument('--'+key, default=FIXTURE/name, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--prose', action='store_true', help='Repeat the six archived prose questions on their first windows')
    parser.add_argument('--baseline', type=Path, default=Path('docs/acceptance/folder-install-935.json'))
    parser.add_argument('--seeds', nargs='+', type=int, default=[17,29,43])
    parser.add_argument('--arms', nargs='+', choices=('native','row','compact','compact_with_tools',
                        'cells','question_only','plain_evidence','no_tools'),
                        default=['native','row','compact','compact_with_tools','cells'])
    main(parser.parse_args())
