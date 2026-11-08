"""JSON/value adapters. Rust owns row normalization and evidence text construction."""
import json
import math
from backend import native_core


def wire(value):
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if isinstance(value, dict):
        return {str(key): wire(item) for key,item in value.items()}
    if isinstance(value, (list,tuple)):
        return [wire(item) for item in value]
    if value is None or isinstance(value,(str,int,float,bool)):
        return value
    return str(value)  # Dates/legacy scalar representations, with no interpretation.


def normalize(raw, mapping, doc_type, doc_title, source_file):
    return native_core.required('tables',request={'action':'normalize',
        'rows':[{'raw':wire(list(raw.items()))}], 'mapping':mapping,
        'doc_type':doc_type,'doc_title':doc_title,'source_file':source_file})['rows'][0]


def chunks(rows, dataset_id=''):
    # Bound only transport batches; row choices, text and metadata belong to Rust.
    batch=[];size=0;result=[]
    for row in rows:
        row=wire(row);length=len(json.dumps(row,ensure_ascii=False,allow_nan=False).encode('utf-8'))
        if batch and (size+length>3*1024*1024 or len(batch)>=512):
            result.extend(native_core.required('tables',request={'action':'chunks','rows':batch,'dataset_id':dataset_id})['chunks'])
            batch=[];size=0
        batch.append(row);size+=length
    if batch:result.extend(native_core.required('tables',request={'action':'chunks','rows':batch,'dataset_id':dataset_id})['chunks'])
    return result
