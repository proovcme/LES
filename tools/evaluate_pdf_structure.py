"""Compare page-anchored evidence from LES and Docling on an isolated PDF corpus."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import time
import unicodedata
from pathlib import Path


def normalize(value: str) -> str:
    return " ".join(unicodedata.normalize("NFKC", value).casefold().split())


def les_elements(path: Path) -> tuple[str, list[dict]]:
    from backend.pdf_reader import extract_markdown

    text, needs_ocr = extract_markdown(path)
    blocks = re.split(r"(?m)^## Стр\. (\d+)\s*$", text)
    elements = []
    for page, block in zip(blocks[1::2], blocks[2::2]):
        for line in block.splitlines():
            if not line.strip():
                continue
            kind = "table" if " | " in line else "text"
            elements.append({"page": int(page), "kind": kind, "text": line.strip()})
    return ("needs_ocr" if needs_ocr else "ok"), elements


def docling_extractor(artifacts: Path | None):
    from docling.datamodel.base_models import InputFormat
    from docling.datamodel.pipeline_options import PdfPipelineOptions
    from docling.document_converter import DocumentConverter, PdfFormatOption

    options = PdfPipelineOptions(enable_remote_services=False, allow_external_plugins=False,
                                 document_timeout=180, artifacts_path=artifacts)
    converter = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=options)})

    def extract(path: Path) -> tuple[str, list[dict]]:
        result = converter.convert(path, raises_on_error=False)
        elements = []
        for item, _ in result.document.iterate_items():
            pages = sorted({prov.page_no for prov in getattr(item, "prov", [])})
            if not pages:
                continue
            kind = str(item.label)
            if kind == "table":
                lines = [" | ".join(cell.text for cell in row) for row in item.data.grid]
            else:
                lines = [getattr(item, "text", "")]
            # A table with several provenance pages cannot assign each row to a
            # specific page from its enclosing element alone.
            if len(pages) != 1:
                continue
            for text in lines:
                if text.strip():
                    elements.append({"page": pages[0], "kind": kind, "text": text})
        return str(result.status), elements

    return extract


def score(elements: list[dict], checks: list[dict]) -> list[dict]:
    scores = []
    for check in checks:
        phrase = normalize(check["text"])
        matched = any(element["page"] == check["page"]
                      and (not check.get("kind") or element["kind"] == check["kind"])
                      and phrase in normalize(element["text"]) for element in elements)
        scores.append({"id": check["id"], "matched": matched})
    return scores


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--engine", choices=("les", "docling", "both"), default="both")
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text(encoding="utf-8-sig"))
    extractors = {}
    if args.engine in ("les", "both"):
        extractors["les"] = les_elements
    if args.engine in ("docling", "both"):
        extractors["docling"] = docling_extractor(args.artifacts)
    rows = []
    for document in manifest["documents"]:
        path = args.manifest.parent / document["path"]
        if path.suffix.lower() != ".pdf":
            raise ValueError("Manifest entries must be PDFs.")
        for engine, extract in extractors.items():
            before = hashlib.sha256(path.read_bytes()).hexdigest()
            start = time.perf_counter()
            status, elements = extract(path)
            elapsed_ms = round((time.perf_counter() - start) * 1000)
            if hashlib.sha256(path.read_bytes()).hexdigest() != before:
                raise RuntimeError("Source changed during the trial.")
            rows.append({"id": document["id"], "sha256": before, "engine": engine, "status": status,
                         "elapsed_ms": elapsed_ms, "elements": len(elements),
                         "checks": score(elements, document["checks"])})
    with args.output.open("x", encoding="utf-8") as output:
        json.dump({"schema": "les.pdf-structure-trial.v1", "runs": rows}, output, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()
