"""Read installed skill packages without putting every reference into context."""
from pathlib import Path
import re

from backend.runtime_paths import mutable_path

_NAME = re.compile(r'^[a-z0-9][a-z0-9_-]{0,100}$')
_TEXT = {'.md', '.txt', '.py', '.json', '.yaml', '.yml', '.csv'}


def library_root():
    return mutable_path('storage/skills').resolve()


def _file(skill, relative):
    if not _NAME.fullmatch(skill):
        raise ValueError('Некорректное имя навыка')
    root = library_root()
    package = (root / skill).resolve()
    path = Path(relative)
    if path.is_absolute() or path.drive or ':' in relative:
        raise ValueError('Нужен относительный путь внутри библиотеки навыков')
    target = (package / path).resolve()
    if not package.is_relative_to(root) or not target.is_relative_to(root):
        raise ValueError('Файл находится вне библиотеки навыков')
    if not (package / 'SKILL.md').is_file() or not target.is_file():
        raise ValueError('Навык или файл не найден')
    if any(part.startswith('.') for part in target.relative_to(root).parts):
        raise ValueError('Скрытые файлы недоступны')
    return target


def catalog(offset=0):
    root = library_root()
    items = []
    for package in sorted(root.iterdir()) if root.exists() else []:
        if not _NAME.fullmatch(package.name):
            continue
        try:
            path = _file(package.name, 'SKILL.md')
            if path.stat().st_size > 1_000_000:
                continue
            text = path.read_text(encoding='utf-8-sig')
        except (OSError, ValueError):
            continue
        description = next((line.partition(':')[2].strip() for line in text.splitlines()
                            if line.startswith('description:')), '')
        items.append({'skill': package.name, 'description': description[:500]})
    offset = max(0, int(offset))
    page = items[offset:offset + 10]
    return {'skills': page, 'total': len(items),
            'next_offset': offset + len(page) if offset + len(page) < len(items) else None}


def read(skill, path='SKILL.md', offset=0):
    target = _file(skill, path)
    if target.suffix.lower() not in _TEXT:
        raise ValueError('Этот файл нельзя прочитать как текст')
    if target.stat().st_size > 1_000_000:
        raise ValueError('Файл навыка слишком большой')
    text = target.read_text(encoding='utf-8-sig')
    offset = max(0, int(offset))
    chunk = text[offset:offset + 6000]
    return {'skill': skill, 'path': target.relative_to(library_root()).as_posix(),
            'text': chunk, 'total_chars': len(text), 'offset': offset,
            'next_offset': offset + len(chunk) if offset + len(chunk) < len(text) else None,
            'arbitrary_code_execution': False}


def register_tools(registry):
    from proxy.services.tool_contract_service import (
        ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
    from proxy.services.tool_registry_service import ToolRegistration
    offset = {'type': 'integer', 'minimum': 0}
    definitions = [
        ('list_installed_skills', 'Библиотека навыков',
         'List installed skill descriptions. Continue with next_offset until null.',
         {'offset': offset}, [], lambda args: catalog(**args)),
        ('read_installed_skill', 'Прочитать навык',
         'Read SKILL.md or a referenced text file in the installed skill library. '
         'Follow next_offset until null to read the entire file. Scripts are readable, not executable.',
         {'skill': {'type': 'string'}, 'path': {'type': 'string'}, 'offset': offset},
         ['skill'], lambda args: read(**args)),
    ]
    for name, title, description, properties, required, action in definitions:
        contract = ToolContract(
            name=name, version='1.0.0', title=title, category='skills', summary=description,
            input_schema={'type': 'object', 'properties': properties, 'required': required,
                          'additionalProperties': False}, result_schema='les_tool_result_v1',
            effect=EffectClass.READ, scopes=('skills',), timeout_seconds=5,
            retry=RetryPolicy.SAFE, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=16000, max_items=12), model_owned_fields=(),
            provenance='User-installed skill library', tags=('skills',))

        def handler(arguments, execute=action, tool_name=name):
            from proxy.services.tool_harness_service import _result
            return _result(tool=tool_name, operation='read_skill', inputs=[], status='ok',
                           result=execute(arguments), trace='installed_skill_library')

        registry.register(ToolRegistration(contract=contract, handler=handler))
