"""Build the pinned native core; no compiler is needed on the user's machine."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def stage_asset(output: Path, source: Path):
    receipt = json.loads((source.parent/'core-receipt.json').read_text(encoding='utf-8'))
    with source.open('rb') as stream:
        checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
    if (receipt.get('schema') != 'les.core-asset.v1' or receipt.get('abi') != 1 or receipt.get('remapped_paths') is not True
            or receipt.get('file') != source.name or receipt.get('sha256') != checksum):
        raise ValueError('Native core asset does not match its receipt')
    output.mkdir(parents=True, exist_ok=False)
    shutil.copy2(source, output/source.name)
    shutil.copy2(source.parent/'core-receipt.json', output/'core-receipt.json')
    return output/source.name


def build(output: Path, *, offline=False):
    if output.exists():
        raise FileExistsError('Use a new output directory to preserve existing assets')
    command = ['cargo', 'build', '--locked', '--release', '--manifest-path', str(ROOT/'core/Cargo.toml')]
    if offline:
        command.append('--offline')
    environment = os.environ.copy()
    flags = (environment['CARGO_ENCODED_RUSTFLAGS'].split('\x1f') if environment.get('CARGO_ENCODED_RUSTFLAGS')
             else environment.get('RUSTFLAGS', '').split())
    for source, replacement in ((Path.home(), 'build-home'),
            (Path(os.getenv('CARGO_HOME', str(Path.home()/'.cargo'))), 'cargo'), (ROOT, 'les')):
        flags.append(f'--remap-path-prefix={source}={replacement}')
    environment['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    subprocess.run(command, check=True, env=environment)
    name = 'les_core.dll' if sys.platform == 'win32' else ('libles_core.dylib' if sys.platform == 'darwin' else 'libles_core.so')
    source = ROOT/'core/target/release'/name
    if not source.is_file():
        raise FileNotFoundError('Native core build did not produce a library for this host')
    output.mkdir(parents=True)
    target = output/name
    shutil.copy2(source, target)
    with target.open('rb') as stream:
        checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
    receipt = {'schema':'les.core-asset.v1', 'abi':1, 'file':name, 'sha256':checksum, 'remapped_paths':True}
    (output/'core-receipt.json').write_text(json.dumps(receipt,indent=2),encoding='utf-8')
    return target


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--offline', action='store_true')
    args = parser.parse_args()
    print(build(args.output.resolve(), offline=args.offline))
