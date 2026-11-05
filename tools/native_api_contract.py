"""Development-only baseline of the actual Light API; never starts services/models."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
OUTPUT=ROOT/'docs/generated/native-api-baseline.json'


def digest(value):
    return hashlib.sha256(json.dumps(value,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()


def capture():
    from proxy.app import create_app
    api=create_app().openapi()
    routes=[]
    for path,methods in sorted(api['paths'].items()):
        if not path.startswith(('/api/','/v1/')):
            continue
        for method,operation in sorted(methods.items()):
            if method not in {'get','post','put','patch','delete','head','options'}:
                continue
            routes.append({'method':method.upper(),'path':path,'operation_id':operation.get('operationId'),
                'request_response_sha256':digest({name:operation.get(name) for name in ('parameters','requestBody','responses','security')})})
    return {'schema':'les.native-api-baseline.v1','product_edition':'light','routes':routes,
            'components_sha256':digest(api.get('components',{}))}


def isolated_capture():
    with tempfile.TemporaryDirectory(prefix='les-api-contract-') as folder:
        state=Path(folder);(state/'data').mkdir();(state/'.env').write_text('',encoding='utf-8')
        environment={key:value for key,value in os.environ.items()
                     if key.upper() in {'SYSTEMROOT','WINDIR','PATH','PATHEXT','TEMP','TMP','USERPROFILE','LOCALAPPDATA','APPDATA','LES_CORE_LIBRARY'}}
        environment.update({'PYTHONUTF8':'1','PYTHONPATH':str(ROOT),'LES_PRODUCT_EDITION':'light',
            'LES_STATE_ROOT':folder,'LES_WINDOWS_STATE_ROOT':folder,'LES_ENV_PATH':str(state/'.env'),
            'RAG_META_DB_PATH':str(state/'data/meta.db'),'LES_STARTUP_BACKGROUND_MUTATIONS':'false',
            'HF_HUB_OFFLINE':'1','TRANSFORMERS_OFFLINE':'1','RAG_TOKENIZER_LOCAL_FILES_ONLY':'true'})
        result=subprocess.run([sys.executable,'-B','-m','tools.native_api_contract','--capture'],
            cwd=ROOT,env=environment,capture_output=True,text=True,encoding='utf-8',timeout=30,check=True)
        return json.loads(result.stdout)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture',action='store_true',help=argparse.SUPPRESS)
    parser.add_argument('--check',action='store_true')
    args=parser.parse_args()
    if args.capture:
        print(json.dumps(capture(),ensure_ascii=False));return 0
    baseline=isolated_capture()
    rendered=json.dumps(baseline,ensure_ascii=False,indent=2)+'\n'
    if args.check:
        if not OUTPUT.is_file() or OUTPUT.read_text(encoding='utf-8')!=rendered:
            print('Light API baseline changed: review compatibility before regenerating');return 1
    else:
        OUTPUT.parent.mkdir(parents=True,exist_ok=True);OUTPUT.write_text(rendered,encoding='utf-8')
    print(f"Light API contract: {len(baseline['routes'])} method/path pairs")
    return 0


if __name__=='__main__':
    raise SystemExit(main())
