"""Stage existing engineering readers as an independent, offline LES asset."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys

SCHEMA = "les.engineering-reader-assets.v1"
RECEIPT = "reader-receipt.json"
MODULE = Path("node_modules/@mlightcad/libredwg-web")
IFC_MODULE = Path("node_modules/web-ifc")
IFC_FILES = ("package.json", "web-ifc-api-node.js", "web-ifc-node.wasm", "LICENSE.md")
IFC_ASSETS = {"ifc-data.cjs", *((IFC_MODULE / name).as_posix() for name in IFC_FILES)}
NODE = "node.exe" if sys.platform == "win32" else "node"
RVT = "ifccs-rvt-metadata.exe" if sys.platform == "win32" else "ifccs-rvt-metadata"


def inventory(root: Path) -> dict[str, str]:
    files = {}
    for item in sorted(root.rglob("*")):
        if item.is_symlink() or not item.resolve().is_relative_to(root):
            raise ValueError("Reader assets must stay within their own directory")
        if item.is_file() and item != root / RECEIPT:
            if item.stat().st_size > 128 * 1024 * 1024:
                raise ValueError("Engineering reader asset exceeds the file limit")
            with item.open("rb") as handle:
                files[item.relative_to(root).as_posix()] = hashlib.file_digest(handle, "sha256").hexdigest()
    if not 0 < len(files) <= 512:
        raise ValueError("Unexpected engineering reader asset count")
    return files


def prepare(output: Path, dwg_reader: Path, node: Path, rvt_reader: Path, notices: Path | None,
            ifc_module: Path | None = None):
    package = dwg_reader.parent / MODULE
    if not all(path.is_file() for path in (dwg_reader, node, rvt_reader, package / "package.json")):
        raise FileNotFoundError("Provide the existing DWG script, its local package, Node and RVT reader")
    if ifc_module is not None and not all((ifc_module / name).is_file() for name in IFC_FILES):
        raise FileNotFoundError("Provide the WebIFC Node package with its WASM and license")
    output.mkdir(parents=True, exist_ok=False)
    shutil.copy2(node, output / NODE)
    shutil.copy2(rvt_reader, output / RVT)
    shutil.copy2(dwg_reader, output / "read-data.mjs")
    for part in ("dist", "wasm"):
        shutil.copytree(package / part, output / MODULE / part)
    shutil.copy2(package / "package.json", output / MODULE / "package.json")
    if ifc_module is not None:
        (output / IFC_MODULE).mkdir(parents=True)
        for name in IFC_FILES:
            shutil.copy2(ifc_module / name, output / IFC_MODULE / name)
        shutil.copy2(Path(__file__).with_name("ifc-data.cjs"), output / "ifc-data.cjs")
    if notices is not None:
        shutil.copytree(notices, output / "notices")
    files = inventory(output.resolve())
    (output / RECEIPT).write_text(json.dumps({"schema": SCHEMA, "files": files}, indent=2) + "\n", encoding="utf-8")


def stage(source: Path, destination: Path):
    source = source.resolve()
    receipt = json.loads((source / RECEIPT).read_text(encoding="utf-8"))
    files = inventory(source)
    required = {NODE, RVT, "read-data.mjs", (MODULE / "package.json").as_posix(),
                (MODULE / "dist/libredwg-web.js").as_posix(), (MODULE / "wasm/libredwg-web.wasm").as_posix()}
    if receipt.get("schema") != SCHEMA or receipt.get("files") != files or not required <= files.keys():
        raise ValueError("Engineering reader assets do not match their receipt")
    if IFC_ASSETS & files.keys() and not IFC_ASSETS <= files.keys():
        raise ValueError("Engineering reader assets contain an incomplete WebIFC reader")
    if not {"notices/Node-LICENSE.txt", "notices/RVT-LICENSE.txt",
            "notices/LibreDWG-LICENSE.txt", "notices/reader-sources.txt"} <= files.keys():
        raise ValueError("Supply third-party notices and the corresponding reader sources before packaging")
    shutil.copytree(source, destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dwg-reader", type=Path, required=True)
    parser.add_argument("--node", type=Path, required=True)
    parser.add_argument("--rvt-reader", type=Path, required=True)
    parser.add_argument("--ifc-module", type=Path)
    parser.add_argument("--notices", type=Path)
    args = parser.parse_args()
    prepare(args.output, args.dwg_reader, args.node, args.rvt_reader, args.notices, args.ifc_module)
