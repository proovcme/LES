"""Execution receipts describe committed ledger state, never model prose."""


def ledger_receipt(kind, state):
    counts = {name: state.get(name, 0) for name in (
        'total', 'pending', 'reviewed', 'needs_review', 'excluded', 'needs_recheck', 'blocked_units')}
    if (any(type(value) is not int or value < 0 for value in counts.values())
            or sum(counts[name] for name in ('pending', 'reviewed', 'needs_review', 'excluded', 'needs_recheck')) != counts['total']):
        raise ValueError('Invalid execution ledger counts')
    status = ('unavailable' if state.get('verification_unavailable') else
              'needs_recheck' if counts['needs_recheck'] or counts['blocked_units'] else
              'incomplete' if counts['pending'] or not counts['total'] else
              'needs_attention' if counts['needs_review'] else 'recorded')
    saved = counts['reviewed'] + counts['needs_review'] + counts['excluded']
    subject = 'строк' if kind == 'table_document' else 'разделов'
    notices = {
        'unavailable': 'Не удалось подтвердить состояние задания. Выполнение действий не подтверждено.',
        'needs_recheck': 'Источник или зависимые выводы изменились. Требуется перепроверка; прежний текст не подтверждает актуальность.',
        'incomplete': f'Проверка не завершена: в журнале сохранены решения для {saved} из {counts["total"]} {subject}. Текст модели не подтверждает выполнение остальных действий.',
        'needs_attention': f'Решения записаны, но {counts["needs_review"]} {subject} требуют проверки пользователем.',
        'recorded': f'В журнале сохранены решения для всех {counts["total"]} {subject}. Это подтверждает запись, а не правильность выводов модели.',
    }
    return dict(schema='les.execution-receipt.v1', kind=kind, task_id=state['task_id'], revision=state['revision'],
                status=status, counts=counts, saved=saved, accepted=False, notice=notices[status])


def summarize(receipts):
    receipts = [receipt for receipt in receipts if receipt is not None]
    if not receipts:
        return None
    incomplete = [receipt for receipt in receipts if receipt['status'] != 'recorded']
    return dict(schema='les.execution-summary.v1', status='incomplete' if incomplete else 'recorded',
                tasks=receipts, notice='\n'.join(receipt['notice'] for receipt in incomplete or receipts), accepted=False)


def local_file_receipt(payload):
    """Only a returned native commit receipt confirms a local agent change."""
    result = payload.get('result') if isinstance(payload, dict) else None
    result = result if isinstance(result, dict) else {}
    recorded = payload.get('status') == 'ok' and result.get('persisted') is True and bool(result.get('revision'))
    path = str(result.get('path') or '')
    label = 'О вас' if path == 'USER.md' else 'Память Леса' if path.endswith('MEMORY.md') else path.split('/', 2)[-1] if path else 'Рабочие файлы'
    return dict(schema='les.execution-receipt.v1', kind='agent_file', task_id=path,
                revision=result.get('revision', ''), status='recorded' if recorded else 'incomplete', accepted=False,
                history_id=result.get('history_id'), operation=payload.get('operation'), changed=result.get('changed') is True,
                notice=(f'Убрано в историю: {label}.' if payload.get('operation') == 'delete' else f'Сохранено: {label}.') if recorded else
                'Изменение памяти или рабочего файла не подтверждено. Проверьте ошибку инструмента и повторите действие.')


def execution_incomplete(trace):
    value = trace.get('execution') if isinstance(trace, dict) else None
    return isinstance(value, dict) and value.get('status') not in {'recorded', 'no_changes'}


def no_local_changes():
    """A server-owned fact, independent of any action claims in generated text."""
    return dict(schema='les.execution-summary.v1', status='no_changes', tasks=[], accepted=False,
                notice='Изменений в памяти и рабочих файлах нет.')
