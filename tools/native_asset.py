"""Shared reproducible asset handling for the native runtime/API build tools."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]
SPECS={
    'runtime':('les-runtime','les.native-runtime-asset.v1','runtime-receipt.json'),
    'api':('les-api','les.native-api-asset.v1','api-receipt.json'),
    'parsers':('les-document-parsers','les.document-parsers-asset.v1','parser-receipt.json'),
}


def source_digest(component,root=ROOT):
    SPECS[component]
    paths=[root/'config/version.json']
    paths.extend(root/'config'/name for name in ('execution-presets.json','chat-profile-defaults.json','builtin-tools.json','lexical-v2.json',
        'model-connection-templates.json','table-row-schema.json','extraction-errors.json','unicode/15.1.0.json') if (root/'config'/name).is_file())
    if component == 'api':
        paths.extend(root/'qdrant_visualizer'/name for name in (
            'index.html', 'forest.css', 'forest.js', 'forest-model.js', 'forest-scene.js',
            'navigation.js', 'forest-mist.svg', 'search.html', 'search.js',
        ) if (root/'qdrant_visualizer'/name).is_file())
    for folder in ('core',component, *(['vector'] if component == 'api' else [])):
        paths.extend(root.joinpath(folder,'src').rglob('*.rs'))
        paths.extend(root.joinpath(folder,'src').rglob('*.sql'))
        paths.extend(root/folder/name for name in ('Cargo.toml','Cargo.lock'))
    digest=hashlib.sha256()
    for path in sorted(paths):
        if path.is_symlink():
            raise ValueError('Native build inputs must not be symlinks')
        digest.update(path.relative_to(root).as_posix().encode('utf-8')+b'\0'+path.read_bytes()+b'\0')
    return digest.hexdigest()


def stage_asset(component,output,source,expected_source):
    _,schema,receipt_name=SPECS[component]
    receipt=json.loads((source.parent/receipt_name).read_text(encoding='utf-8'))
    if (receipt.get('schema')!=schema or receipt.get('file')!=source.name
            or receipt.get('sha256')!=hashlib.sha256(source.read_bytes()).hexdigest()
            or receipt.get('source_sha256')!=expected_source or receipt.get('remapped_paths') is not True
            or component=='parsers' and receipt.get('revision')!='les.parsers.v1'):
        raise ValueError('Native asset does not match its receipt/source')
    output.mkdir(parents=True,exist_ok=False)
    shutil.copy2(source,output/source.name);shutil.copy2(source.parent/receipt_name,output/receipt_name)
    return output/source.name


def build(component,output,*,offline=False,features=()):
    if output.exists():
        raise FileExistsError('Use a fresh output directory to preserve rollback assets')
    name,schema,receipt_name=SPECS[component]
    if set(features) - ({'edge'} if component == 'api' else set()):
        raise ValueError('Unsupported native build feature')
    environment=os.environ.copy()
    environment['CARGO_INCREMENTAL']='0'
    flags=(environment['CARGO_ENCODED_RUSTFLAGS'].split('\x1f') if environment.get('CARGO_ENCODED_RUSTFLAGS')
           else environment.get('RUSTFLAGS','').split())
    for source,replacement in ((Path.home(),'build-home'),
            (Path(os.getenv('CARGO_HOME',str(Path.home()/'.cargo'))),'cargo'),(ROOT,'les')):
        flags.append(f'--remap-path-prefix={source}={replacement}')
    environment['CARGO_ENCODED_RUSTFLAGS']='\x1f'.join(flags)
    command=['cargo','build','--locked','--release','-j','1','--manifest-path',str(ROOT/component/'Cargo.toml')]
    if offline:
        command.append('--offline')
    if features:
        command.extend(['--features', ','.join(sorted(set(features)))])
    subprocess.run(command,env=environment,check=True)
    file=name+('.exe' if sys.platform=='win32' else '')
    target=Path(environment.get('CARGO_TARGET_DIR', str(ROOT/component/'target')))
    source=target/'release'/file
    if not source.is_file():
        raise FileNotFoundError('Native build did not produce its executable')
    output.mkdir(parents=True);shutil.copy2(source,output/file)
    version=json.loads((ROOT/'config/version.json').read_text(encoding='utf-8'))
    receipt={'schema':schema,'file':file,'build_number':version['build_number'],
             'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
             'source_sha256':source_digest(component),'remapped_paths':True}
    if features:
        receipt['features']=sorted(set(features))
    if component == 'parsers':
        receipt['revision']='les.parsers.v1'
    (output/receipt_name).write_text(json.dumps(receipt,indent=2),encoding='utf-8')
    return output/file


if __name__=='__main__':
    import argparse
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('component',choices=sorted(SPECS))
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--offline',action='store_true')
    args=parser.parse_args()
    print(build(args.component,args.output.resolve(),offline=args.offline))
