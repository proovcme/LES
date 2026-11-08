"""Snapshot IO for the built-in Rust workbook calculator. No arithmetic here."""
import json
from backend import native_core


class WorkbookCalculationError(ValueError):
    def __init__(self, code, message):
        self.code=code
        super().__init__(message)


def execute(document, *, action, ranges=None):
    if not native_core.supports('workbook_calculation'):
        raise WorkbookCalculationError('WORKBOOK_CALCULATOR_MISSING',
            'Компонент расчёта книги отсутствует. Восстановите установку Леса.')
    encoded=json.dumps(document,ensure_ascii=False,separators=(',',':'),allow_nan=False)
    cursor=0
    def exchange(operation, value):
        nonlocal cursor
        if operation!=0 or value.get('offset')!=cursor or value.get('limit')!=262144:
            raise WorkbookCalculationError('WORKBOOK_SNAPSHOT_INVALID','Не удалось передать книгу для расчёта')
        text=encoded[cursor:cursor+262144];cursor+=len(text)
        return {'text':text,'complete':cursor>=len(encoded)}
    try:
        result=native_core.run_workbook({'schema':'les.workbook-calculation.v1',
            'action':action,'ranges':ranges},exchange)
    except native_core.NativeCoreError as error:
        raise WorkbookCalculationError(str(error),'Не удалось проверить книгу и вычислить сумму') from error
    if result.get('ok') is not True:
        error=result.get('error',{})
        raise WorkbookCalculationError(error.get('code','WORKBOOK_CALCULATION_FAILED'),
                                      error.get('message','Не удалось вычислить сумму'))
    return result['value']
