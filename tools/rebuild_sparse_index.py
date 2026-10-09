"""Inspect or explicitly rebuild lexical weights; never calls a model."""
import argparse
from contextlib import closing
import json
import logging
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from qdrant_client import QdrantClient
from backend.index_replacement import ReplacementJournal
from backend.light_qdrant_connection import qdrant_client_options
from backend.sparse_index import ensure_current, read_contract


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--collection", required=True)
    parser.add_argument("--content-dir", required=True, type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--mode", choices=("bm25", "tf"), default="bm25")
    args = parser.parse_args()
    logging.basicConfig(level=logging.INFO)
    journal = ReplacementJournal(args.content_dir, args.collection)
    if not args.apply:
        print(json.dumps(dict(contract=read_contract(journal), pending=journal.pending()), ensure_ascii=False))
        return
    with closing(QdrantClient(url=args.url, timeout=60, check_compatibility=False,
                              **qdrant_client_options(args.url))) as client:
        result = ensure_current(client, journal, mode=args.mode)
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
