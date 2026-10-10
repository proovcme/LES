"""Install a local browser-edition preview, not a standalone signed distribution."""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys

from tools.build_light_package import stage_code
from tools.light_runtime_assets import stage, host_target


def install(app: Path, state: Path) -> Path:
    if sys.platform != 'darwin':
        raise RuntimeError('This installer requires macOS')
    app = app.expanduser().resolve()
    state = state.expanduser().resolve()
    if app.exists():
        raise FileExistsError('Application already exists; use another --app path to preserve rollback')
    uv = shutil.which('uv')
    if not uv:
        raise RuntimeError('Install uv before running this developer installer')
    resources = app / 'Contents/Resources'
    resources.mkdir(parents=True)
    stage_code(resources)
    runtime = resources / 'runtime'
    # Reuse only the locally checksum-verified download, or fetch the pinned asset.
    source=Path(__file__).resolve().parents[1]/'native/qdrant'
    receipt=source/'asset-receipt.json'
    if receipt.is_file() and (source/'qdrant').is_file():
        data=json.loads(receipt.read_text())
        contract=json.loads((runtime/'config/light-runtime.json').read_text())['qdrant']['platforms'][host_target()]
        if data.get('sha256') != contract['sha256'] or hashlib.sha256((source/'qdrant').read_bytes()).hexdigest() != data.get('executable_sha256'):
            raise ValueError('Cached Qdrant verification failed')
        target=runtime/'native/qdrant';target.mkdir(parents=True)
        shutil.copy2(source/'qdrant',target/'qdrant');shutil.copy2(receipt,target/receipt.name)
    else:
        stage(runtime / 'native/qdrant', target=host_target())
    vision=runtime/'native/vision';vision.mkdir(parents=True)
    subprocess.run(['/usr/bin/xcrun','swiftc',str(Path(__file__).with_name('light_macos_ocr.swift')),
                    '-o',str(vision/'les-ocr')],check=True)
    subprocess.run([uv, 'sync', '--locked', '--no-dev', '--extra', 'mcp'], cwd=runtime, check=True)
    # Installer writes paths into a local-only plist; no machine paths in source.
    version = json.loads((runtime/'config/version.json').read_text())
    contents = app / 'Contents'
    (contents/'Info.plist').write_bytes(plistlib.dumps({
        'CFBundleName':'LES RAG Preview', 'CFBundleDisplayName':'LES RAG Preview',
        'CFBundleIdentifier':'me.ovc.les-light.macos-preview',
        'CFBundleExecutable':'LES', 'CFBundlePackageType':'APPL',
        'CFBundleShortVersionString':version['product_version'],
        'CFBundleVersion':str(version['build_number']),
        'LSUIElement':True,
    }))
    (resources/'state-path.txt').write_text(str(state))
    executable = contents/'MacOS/LES';executable.parent.mkdir()
    executable.write_text('''#!/bin/sh
set -eu
resources="$(CDPATH= cd -- "$(dirname -- "$0")/../Resources" && pwd)"
state="$(cat "$resources/state-path.txt")"
mkdir -p "$state/logs"
cd "$resources/runtime"
exec .venv/bin/python -B -m tools.light_launcher --state "$state" --browser >>"$state/logs/launcher-console.txt" 2>&1
''')
    executable.chmod(0o755)
    return app


def main(argv=None):
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app',type=Path,default=Path.home()/'Applications/LES RAG Preview.app')
    parser.add_argument('--state',type=Path,default=Path.home()/'Library/Application Support/LES Light Mac Preview')
    args=parser.parse_args(argv)
    print(install(args.app,args.state))


if __name__=='__main__':
    main()
