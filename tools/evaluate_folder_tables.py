"""Compare three local readers on verified PDF rows and optionally on specified Excel rows."""
import argparse
from hashlib import file_digest
import json
from pathlib import Path

from tools.evaluate_folder_window import FIXTURE, evidence, generate
from tools.folder_cell_reader import (
    Row, literal_request, quote_request, resolve, resolve_literals, resolve_quote, select_request,
)


def pdf_cases():
    archive = json.loads((FIXTURE.parent / 'folder-install-935.json').read_bytes())
    for condition in ('present', 'removed'):
        capture = json.loads((FIXTURE / f'{condition}.json').read_bytes())
        excerpts = evidence(capture, FIXTURE / condition)
        # The authored physical grid has one table and its headings occupy line 1.
        header = next(item['text'] for item in excerpts if item['line'] == 1)
        headers = dict(zip('ABCD', header.strip('|').split('|'), strict=True))
        items, expected = [], None
        for item in excerpts:
            source = item['id']
            if item['line'] and item['line'] != 1:
                cells = dict(zip(headers, item['text'].strip('|').split('|'), strict=True))
                items.append(Row(source, cells, headers))
                if item['text'] == archive['expected_row']:
                    expected = cells['D'], cells['C'], source
            else:
                items.append({'источник': f'S{source}', 'текст': item['text']})
        if (expected is None) != (condition == 'removed'):
            raise ValueError('Unexpected PDF fixture')
        yield 'pdf-' + condition, archive['question'], items, expected


def workbook_cases(manifest):
    from openpyxl import load_workbook
    from openpyxl.utils import get_column_letter

    rows = []
    specs = json.loads(manifest.read_bytes())['rows']
    for spec in specs:
        filename = Path(spec['path'])
        with filename.open('rb') as stream:
            if file_digest(stream, 'sha256').hexdigest() != spec['sha256']:
                raise ValueError('Original workbook changed')
            stream.seek(0)
            book = load_workbook(stream, read_only=True, data_only=True)
            try:
                sheet = book[spec['sheet']]
                def read_line(number):
                    return next(sheet.iter_rows(min_row=number, max_row=number,
                                                max_col=spec['max_column'], values_only=True))
                headers = {get_column_letter(j): ' '.join(str(h).split())
                           for j, h in enumerate(read_line(spec['header_row']), 1) if h is not None}
                cells = {get_column_letter(j): str(v) for j, v in enumerate(read_line(spec['row']), 1)
                         if v is not None and get_column_letter(j) in headers}
            finally:
                book.close()
        rows.append(Row(spec['source'], cells, headers))
    if len({row.source for row in rows}) != len(rows):
        raise ValueError('Duplicate source IDs')
    for i, (row, spec) in enumerate(zip(rows, specs, strict=True), 1):
        name = row.cells[spec['item_column']]
        if any(other != row and name in other.cells.values() for other in rows):
            raise ValueError('Ambiguous question in the selected window')
        unit = spec['unit_column']
        question = f'Какое количество и какая единица измерения указаны для позиции «{name}»?'
        expected = row.cells[spec['value_column']], row.cells[unit], row.source
        yield f'workbook-{i}-present', question, rows, expected
        yield f'workbook-{i}-removed', question, [other for other in rows if other != row], None
        sparse = Row(row.source, {k: v for k, v in row.cells.items() if k != unit}, row.headers)
        yield f'workbook-{i}-missing-unit', question, [sparse if other == row else other for other in rows], None


READERS = {'cell': (select_request, resolve), 'literal': (literal_request, resolve_literals),
           'quote': (quote_request, resolve_quote)}


def main(args):
    cases = list(pdf_cases())
    if args.workbooks:
        cases.extend(workbook_cases(args.workbooks))
    body = json.loads((FIXTURE / 'present.json').read_bytes())['captures'][0]['body']
    prepare, read = READERS[args.reader]
    with args.output.open('x', encoding='utf8') as out:
        for seed in args.seeds:
            for case, question, items, expected in cases:
                request = prepare(body, question, items)
                request['options'] = {**body['options'], 'seed': seed}
                raw, calls, end = generate(request)
                try:
                    result = read(raw, [r for r in items if isinstance(r, Row)])
                    status = 'abstained' if result is None else 'selected'
                    correct = result == expected and not calls and end.get('done_reason') == 'stop'
                except ValueError:
                    status, correct = 'invalid', False
                # The result may be shared; exclude the workbook contents and provider response.
                result = dict(case=case, seed=seed, status=status, correct=correct,
                              input_tokens=end.get('prompt_eval_count'))
                print(json.dumps(result), file=out, flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reader', choices=READERS, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--seeds', type=int, nargs='+', default=[17, 29, 43])
    parser.add_argument('--workbooks', type=Path, help='Explicit local manifest of workbook rows with pinned SHA-256')
    main(parser.parse_args())
