"""Built-in workbook arithmetic uses Rust; legacy scripts retain their own runtime."""
from decimal import Decimal, Inexact, localcontext
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess

import pytest
from backend import native_core
from backend.workbook_calculation import execute, WorkbookCalculationError


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native built-in calculation asset required')
    assert native_core.supports('workbook_calculation')


def snapshot(values, *, sheet='Лист 日本語'):
    return {'schema':'les.table.v1','sha256':'0'*64,
        'sheets':[{'name':sheet,'columns':2,'hidden':False,'merged_ranges':[]}],
        'rows':[{'sheet':sheet,'row':i,'cells':[{'coordinate':f'A{i}','kind':'number','value':v}]}
                for i,v in enumerate(values,1)]}


def sum_values(values):
    return execute(snapshot(values),action='sum',ranges=[{'sheet':'Лист 日本語','range':f'A1:A{len(values)}'}])


def oracle(values):
    with localcontext() as context:
        context.prec=1024;context.traps[Inexact]=True
        total=sum((Decimal(v) for v in values),Decimal(0)).normalize()
        return format(total,'f') if -12<=total.adjusted()<=20 else str(total)


def test_exact_results_match_decimal_for_2200_deterministic_inputs():
    rng=random.Random(827)
    for _ in range(2000):
        values=[f'{rng.randrange(-10**24,10**24)}e{rng.randrange(-324,309)}' for _ in range(rng.randrange(1,6))]
        assert sum_values(values)['total']==oracle(values),values
    for _ in range(200):
        integer=rng.randrange(-10**40,10**40)
        values=[str(integer),str(-integer),f'{rng.randrange(-1000,1000)}e-15']
        assert sum_values(values)['total']==oracle(values)


@pytest.mark.parametrize('values',[
    ['0.1']*600+['.2'],['-0','0e100'],['1e999999','-1e999999'],
    ['1e-1001022'],['1e21'],['1e20'],['1.2e-12'],['1.2e-13'],['9e308','-8e308']])
def test_decimal_extremes_zero_cancellation_and_display_thresholds(values):
    assert sum_values(values)['total']==oracle(values)


@pytest.mark.parametrize('literal',['NaN','Infinity','-Infinity','1e2147483647','1e-2147483648','1_000',''])
def test_nonfinite_or_malformed_numbers_never_return_a_total(literal):
    with pytest.raises(WorkbookCalculationError):sum_values(['1',literal])


def test_inexact_sum_refuses_instead_of_rounding():
    with localcontext() as ctx:
        ctx.prec=1024;ctx.traps[Inexact]=True
        with pytest.raises(Inexact):sum((Decimal(v) for v in ['1e1024','1']),Decimal(0))
    with pytest.raises(WorkbookCalculationError) as fault:sum_values(['1e1024','1'])
    assert fault.value.code=='WORKBOOK_SUM_PRECISION'


def test_duplicate_coordinates_and_mismatched_rows_cannot_double_count():
    doc=snapshot(['1']);doc['rows'][0]['cells']*=2
    with pytest.raises(WorkbookCalculationError):execute(doc,action='sum',ranges=[{'sheet':'Лист 日本語','range':'A1'}])
    doc=snapshot(['1']);doc['rows'][0]['cells'][0]['coordinate']='A2'
    with pytest.raises(WorkbookCalculationError):execute(doc,action='sum',ranges=[{'sheet':'Лист 日本語','range':'A1'}])


def test_formula_cache_and_describe_preserve_provenance():
    doc=snapshot(['1','2']);cell=doc['rows'][1]['cells'][0]
    cell.update(kind='formula',value='=A1+1',cached={'kind':'number','value':'2'},needs_recalculation=False)
    result=execute(doc,action='sum',ranges=[{'sheet':'Лист 日本語','range':'$A$1:$A$2'}])
    assert result['total']=='3' and result['ranges'][0]['cached_formulas']==1
    assert result['source_sha256']==doc['sha256'] and result['ranges'][0]['range']=='A1:A2'
    assert execute(doc,action='describe')=={'sheets':[{'name':'Лист 日本語','last_row':2,'last_column':'B','hidden':False}]}
    cell['needs_recalculation']=True
    with pytest.raises(WorkbookCalculationError) as fault:execute(doc,action='sum',ranges=[{'sheet':'Лист 日本語','range':'A1:A2'}])
    assert fault.value.code=='WORKBOOK_FORMULA_CACHE_MISSING'


def test_large_unicode_snapshot_crosses_generic_input_limit_without_losing_selected_cell():
    doc=snapshot(['1'])
    doc['rows']=[{'sheet':'Лист 日本語','row':i,'cells':[{'coordinate':f'B{i}','kind':'text','value':'日本語'*1400}]} for i in range(1,701)]
    doc['rows'][0]['cells'].append({'coordinate':'A1','kind':'number','value':'0.2'})
    assert len(json.dumps(doc,ensure_ascii=False).encode())>8*1024*1024
    assert execute(doc,action='sum',ranges=[{'sheet':'Лист 日本語','range':'A1'}])['total']=='0.2'


def test_standalone_builtin_reads_multisheet_workbook_with_python_unavailable(tmp_path):
    source=os.getenv('LES_TEST_NATIVE_API')
    if not source:pytest.skip('Native API executable required for standalone built-in test')
    from openpyxl import Workbook
    path=tmp_path/'Книга 日本語 с пробелами.xlsx';book=Workbook();book.active.title='Первый'
    for i in range(1,71):book.active.cell(i,1,.1)
    book.create_sheet('Второй')['B2']=.2;book.save(path);book.close()
    original=path.read_bytes();executable=tmp_path/'Встроенный расчёт.exe';shutil.copy2(source,executable)
    environment={k:v for k,v in os.environ.items() if k.upper() in {'SYSTEMROOT','WINDIR','TEMP','TMP'}}
    environment.update(PATH=str(tmp_path/'no-programs'),PYTHONHOME=str(tmp_path/'no-python'),
        PYTHONPATH=str(tmp_path/'no-python-modules'),LES_SKILL_PYTHON=str(tmp_path/'missing-python.exe'))
    request={'schema':'les.workbook-calculation.v1','action':'sum','path':str(path),
        'ranges':[{'sheet':'Первый','range':'A1:A70'},{'sheet':'Второй','range':'B2'}]}
    result=subprocess.run([str(executable),'--builtin-workbook'],input=json.dumps(request),text=True,
        encoding='utf-8',capture_output=True,cwd=tmp_path,env=environment,timeout=20,
        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert result.returncode==0,result.stderr
    value=json.loads(result.stdout)
    assert value['ok'] and value['owner']=='rust.workbook-calculation.v1',value
    assert value['value']['total']=='7.2' and [r['sum'] for r in value['value']['ranges']]==['7','0.2']
    assert value['value']['source_sha256']==hashlib.sha256(original).hexdigest()
    assert path.read_bytes()==original
