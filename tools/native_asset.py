"""Shared reproducible asset handling for the native runtime/API build tools."""
import hashlib
import json
import os
from pathlib import Path
import re
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
    root = root.resolve()
    paths = {root/'config/version.json'}
    for folder in ('core',component, *(['vector'] if component == 'api' else [])):
        crate = root / folder
        sources = list((crate/'src').rglob('*.rs'))
        if (crate/'build.rs').is_file():
            sources.append(crate/'build.rs')
        paths.update(sources)
        paths.update(crate/name for name in ('Cargo.toml','Cargo.lock'))
        for source in sources:
            contents = source.read_text(encoding='utf-8')
            for macro in re.finditer(r'\binclude_(?:str|bytes)!\s*\(', contents):
                literal = re.match(r'\s*"([^"\\]+)"\s*,?\s*\)', contents[macro.end():])
                if not literal:
                    raise ValueError(f'Unsupported embedded path in {source.relative_to(root)}')
                embedded = (source.parent / literal[1]).resolve()
                if not embedded.is_relative_to(root):
                    raise ValueError('Embedded build input outside repository')
                paths.add(embedded)
    digest=hashlib.sha256()
    for path in sorted(paths):
        if path.is_symlink():
            raise ValueError('Native build inputs must not be symlinks')
        digest.update(path.relative_to(root).as_posix().encode('utf-8')+b'\0'+path.read_bytes()+b'\0')
    return digest.hexdigest()


def stage_asset(component,output,source,expected_source,*,features=()):
    _,schema,receipt_name=SPECS[component]
    receipt=json.loads((source.parent/receipt_name).read_text(encoding='utf-8'))
    if (receipt.get('schema')!=schema or receipt.get('file')!=source.name
            or receipt.get('sha256')!=hashlib.sha256(source.read_bytes()).hexdigest()
            or receipt.get('source_sha256')!=expected_source or receipt.get('remapped_paths') is not True
            or receipt.get('features',[])!=sorted(set(features))
            or component=='parsers' and receipt.get('revision')!='les.parsers.v1'):
        raise ValueError('Native asset does not match its receipt/source')
    output.mkdir(parents=True,exist_ok=False)
    shutil.copy2(source,output/source.name);shutil.copy2(source.parent/receipt_name,output/receipt_name)
    return output/source.name


def remapped_rustflags():
    encoded = os.getenv('CARGO_ENCODED_RUSTFLAGS')
    flags = encoded.split('\x1f') if encoded else os.getenv('RUSTFLAGS', '').split()
    for source, replacement in ((Path.home(), 'build-home'),
            (Path(os.getenv('CARGO_HOME', str(Path.home() / '.cargo'))), 'cargo'), (ROOT, 'les')):
        flag = f'--remap-path-prefix={source}={replacement}'
        if flag not in flags:
            flags.append(flag)
    return '\x1f'.join(flags)


def build(component,output,*,offline=False,features=()):
    if output.exists():
        raise FileExistsError('Use a fresh output directory to preserve rollback assets')
    name,schema,receipt_name=SPECS[component]
    if set(features) - ({'edge'} if component == 'api' else set()):
        raise ValueError('Unsupported native build feature')
    environment=os.environ.copy()
    environment['CARGO_INCREMENTAL']='0'
    environment.setdefault('CARGO_BUILD_JOBS', '1')
    environment['CARGO_ENCODED_RUSTFLAGS'] = remapped_rustflags()
    command=['cargo','build','--locked','--release','--manifest-path',str(ROOT/component/'Cargo.toml')]
    if offline:
        command.append('--offline')
    if features:
        command.extend(['--features', ','.join(sorted(set(features)))])
    inputs = source_digest(component)
    subprocess.run(command,env=environment,check=True)
    if source_digest(component) != inputs:
        raise ValueError('Native build inputs changed during compilation')
    file=name+('.exe' if sys.platform=='win32' else '')
    target=Path(environment.get('CARGO_TARGET_DIR', str(ROOT/component/'target')))
    source=target/'release'/file
    if not source.is_file():
        raise FileNotFoundError('Native build did not produce its executable')
    output.mkdir(parents=True);shutil.copy2(source,output/file)
    version=json.loads((ROOT/'config/version.json').read_text(encoding='utf-8'))
    receipt={'schema':schema,'file':file,'build_number':version['build_number'],
             'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
             'source_sha256':inputs,'remapped_paths':True}
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
