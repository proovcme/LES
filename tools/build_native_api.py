"""Build/stage the native API. No compiler is needed in the user's installation."""
import argparse
from pathlib import Path
import sys
if not __package__:
    sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from tools import native_asset


def source_digest(root=native_asset.ROOT):
    return native_asset.source_digest('api',root)


def stage_asset(output,source):
    return native_asset.stage_asset('api',output,source,source_digest())


def build(output,*,offline=False,edge=False):
    return native_asset.build('api',output,offline=offline,features=('edge',) if edge else ())


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--offline',action='store_true')
    parser.add_argument('--edge',action='store_true')
    args=parser.parse_args();print(build(args.output.resolve(),offline=args.offline,edge=args.edge))
