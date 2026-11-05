"""Build/stage the native service owner, with a versioned receipt and no overwrite."""
import argparse
from pathlib import Path
import sys
if not __package__:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from tools import native_asset

ROOT = native_asset.ROOT


def source_digest(root=ROOT):
    return native_asset.source_digest('runtime', root)


def stage_asset(output, source):
    return native_asset.stage_asset('runtime', output, source, source_digest())


def build(output, *, offline=False):
    return native_asset.build('runtime', output, offline=offline)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--offline', action='store_true')
    arguments = parser.parse_args()
    print(build(arguments.output.resolve(), offline=arguments.offline))
