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


def execution_incomplete(trace):
    value = trace.get('execution') if isinstance(trace, dict) else None
    return isinstance(value, dict) and value.get('status') != 'recorded'
