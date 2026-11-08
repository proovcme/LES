"""Private file/worker binding; Rust owns Parquet encoding and decoding."""
import json
import logging
import os
from pathlib import Path
import tempfile
from backend import native_core
from backend.native_table_rows import wire

logger=logging.getLogger(__name__)


def call(request):
    program=os.getenv('LES_NATIVE_TABLE_WORKER') or os.getenv('LES_NATIVE_FORMAT_WORKER')
    if not program:
        # Direct library consumers remain native; the installed runtime always
        # assigns the supervised executable. No Python/Arrow data reader fallback.
        return native_core.required('tables',request=request)
    response=native_core.required('worker_tables',request={
        'schema':'les.worker-host.v1','program':program,'args':['--serve-tables'],
        'directory':str(Path(program).parent),'context':'les.tables.v1','timeout_ms':120_000,'input':request})
    result=response.get('result',{})
    if response.get('status')!='completed' or result.get('status')!='completed':
        code=result.get('code') or response.get('code') or 'PARQUET_READ_FAILED'
        logger.warning('Native table operation failed code=%s pid=%s exit=%s diagnostics=%s',
            code,response.get('worker_pid'),response.get('exit_code'),str(response.get('diagnostics') or '')[-4000:])
        raise native_core.NativeCoreError(code)
    return result['value']


def write(rows,path):
    if not rows:return 0
    path=Path(path).resolve()
    pending=None
    try:
        with tempfile.NamedTemporaryFile(mode='w',encoding='utf-8',newline='\n',
                dir=path.parent,prefix='.les-parquet-rows-',suffix='.jsonl',delete=False) as stream:
            pending=Path(stream.name)
            for row in rows:stream.write(json.dumps(wire(row),ensure_ascii=False,allow_nan=False,separators=(',',':'))+'\n')
            stream.flush();os.fsync(stream.fileno())
        return call({'action':'write','path':str(path),'rows_path':str(pending)})['rows']
    finally:
        if pending is not None:pending.unlink(missing_ok=True)


def sum_rows(rows,field):
    # Only frame large input; no Python numeric interpretation or addition.
    partial=[];count=0;missing=0
    for start in range(0,len(rows),512):
        value=call({'action':'sum_rows','rows':wire(rows[start:start+512]),'field':field})
        partial.append({field:value['exact_total']});count+=value['count'];missing+=value['missing']
    value=call({'action':'sum_rows','rows':partial,'field':field})
    value.update(count=count,missing=missing,complete=missing==0)
    return value


def read_rows(path,checksum=None):
    offset=0;rows=[]
    while True:
        request={'action':'read','path':str(Path(path).resolve()),'offset':offset,'limit':2000}
        if checksum:request['source_sha256']=checksum
        result=call(request);checksum=result['source_sha256'];rows.extend(result['rows'])
        next_offset=result['next_offset']
        if next_offset is None:return rows
        if next_offset<=offset:raise native_core.NativeCoreError('PARQUET_RESPONSE_INVALID')
        offset=next_offset


def read_columns(path):
    columns={}
    for row in read_rows(path):
        for key,value in row.items():columns.setdefault(key,[]).append(value)
    return columns


def references(dataset_ids,storage_root):
    from backend.rag_config import rag_meta_db_path
    return native_core.required('tables',request={'action':'references',
        'database':str(Path(rag_meta_db_path()).resolve()),'storage_root':str(Path(storage_root).resolve()),
        'dataset_ids':dataset_ids})['refs']
