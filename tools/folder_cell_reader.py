"""Experimental terminal reader for already verified table rows, used in local trials."""
from dataclasses import dataclass
import json


@dataclass(frozen=True)
class Row:
    source: int
    cells: dict[str, str]
    headers: dict[str, str]


def select_request(body, question, evidence):
    """Evidence order and source IDs come from the original retrieval window."""
    rows = [item for item in evidence if isinstance(item, Row)]
    columns = list(dict.fromkeys(key for row in rows for key in row.cells))
    sources = [f'S{row.source}' for row in rows]
    payload = []
    for item in evidence:
        if isinstance(item, Row):
            fields = {key: {'поле': item.headers[key], 'текст': value} for key, value in item.cells.items()}
            payload.append({'строка': f'S{item.source}', 'ячейки': fields})
        else:
            payload.append(item)
    system = ('Выбери строку и два адреса ячеек из неё для ответа на вопрос. row — адрес строки, '
              'value — буква столбца с запрошенным значением, unit — буква столбца с единицей. '
              'Если строка, значение или единица отсутствуют, выбери NONE во всех трёх полях.')
    allowed = {'row': sources, 'value': columns, 'unit': columns}
    schema = {
        'type': 'object',
        'properties': {key: {'type': 'string', 'enum': values + ['NONE']} for key, values in allowed.items()},
        'required': list(allowed),
        'additionalProperties': False,
    }
    messages = [
        {'role': 'system', 'content': system},
        {'role': 'user', 'content': json.dumps(payload, ensure_ascii=False) + '\n\n' + question},
    ]
    req = dict(body, messages=messages, format=schema)
    req.pop('tools', None)
    return req


def resolve(raw, rows):
    address = json.loads(raw)
    if not isinstance(address, dict) or set(address) != {'row', 'value', 'unit'}:
        raise ValueError('Invalid address fields')
    source, value, unit = (address[k] for k in ('row', 'value', 'unit'))
    if source == value == unit == 'NONE':
        return None
    row = next((row for row in rows if source == f'S{row.source}'), None)
    if (row is None or not isinstance(value, str) or not isinstance(unit, str) or value == unit
            or not row.cells.get(value, '').strip() or not row.cells.get(unit, '').strip()):
        raise ValueError('Address outside delivered cells')
    return row.cells[value], row.cells[unit], row.source


def labeled_lines(evidence):
    lines = []
    for item in evidence:
        if isinstance(item, Row):
            fields = ' | '.join(f'{item.headers[key]} = {value}' for key, value in item.cells.items())
            lines.append(f'[Источник {item.source}] {fields}')
        else:
            lines.append(json.dumps(item, ensure_ascii=False))
    return '\n'.join(lines)


def literal_request(body, question, evidence):
    task = ('Ответь по таблице. Если нашёл именно запрошенную позицию, её значение и единицу, '
            'скопируй из одной строки значение и единицу в точности как они написаны. '
            'Верни номер источника этой строки. Если любой из этих фактов отсутствует, верни пустой массив [].')
    fields = {'значение': {'type': 'string'}, 'единица': {'type': 'string'}, 'источник': {'type': 'integer'}}
    schema = {'type': 'array', 'maxItems': 1, 'items': {
        'type': 'object', 'properties': fields, 'required': list(fields), 'additionalProperties': False,
    }}
    req = dict(body, format=schema, messages=[
        {'role': 'system', 'content': task},
        {'role': 'user', 'content': labeled_lines(evidence) + '\n\n' + question},
    ])
    req.pop('tools', None)
    return req


def resolve_literals(raw, rows):
    claims = json.loads(raw)
    if claims == []:
        return None
    if not isinstance(claims, list) or len(claims) != 1 or not isinstance(claims[0], dict):
        raise ValueError('Expected a single claim or an empty list')
    claim = claims[0]
    if set(claim) != {'значение', 'единица', 'источник'} or type(claim['источник']) is not int:
        raise ValueError('Invalid claim')
    return locate_literals(claim['значение'], claim['единица'], claim['источник'], rows)


def locate_literals(value_text, unit_text, source, rows):
    row = next((r for r in rows if r.source == source), None)
    if row is None:
        raise ValueError('Unknown source')
    columns = []
    for value in (value_text, unit_text):
        if not isinstance(value, str) or not value.strip():
            raise ValueError('Empty literal')
        coordinates = [col for col, text in row.cells.items() if text == value]
        if len(coordinates) != 1:
            raise ValueError('Literal missing or ambiguous')
        columns.append(coordinates[0])
    if columns[0] == columns[1]:
        raise ValueError('Value and unit have the same address')
    return row.cells[columns[0]], row.cells[columns[1]], row.source


def quote_request(body, question, evidence):
    task = (
        'Прочитай строки и найди точное название из вопроса. Скопируй значение, единицу и номер '
        'источника. Формат ответа показан ниже. Если указан другой объект, нет значения или '
        'нет единицы измерения, ответь только НЕТ.\n'
        'Пример строки: [Источник 7] Изделие = Шнур | Ед. = м | Кол. = 35\n'
        'Вопрос: Сколько шнура? Ответ: 35 | м | 7\n'
        'Вопрос: Сколько ленты? Ответ: НЕТ\n'
        'Верни одну строку без объяснений.')
    req = dict(body, messages=[
        {'role': 'system', 'content': task},
        {'role': 'user', 'content': labeled_lines(evidence) + '\n\n' + question},
    ])
    req.pop('tools', None)
    return req


def resolve_quote(raw, rows):
    if raw.strip() == 'НЕТ':
        return None
    parts = [s.strip() for s in raw.strip().split('|')]
    if len(parts) != 3 or not parts[2].isascii() or not parts[2].isdecimal():
        raise ValueError('Invalid literal answer')
    value, unit, source = parts
    return locate_literals(value, unit, int(source), rows)
