"""Keep public and in-app Light help identical, without copying private docs."""
import argparse
import json
from pathlib import Path

from sovushka.components.light_guide import ARTICLES


ROOT = Path(__file__).resolve().parents[1]
PUBLIC = ROOT / "docs/public/les-light"


def outputs():
    version = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))["product_version"]
    guide = [f"# LES RAG {version} — руководство пользователя", "", "Первый выпуск LES RAG для Windows. Установщики и результаты проверки доступны в официальном GitHub Releases.", "", "Это тот же текст, который доступен внутри приложения через **Руководство**.", ""]
    previous = None
    for _, category, title, body in ARTICLES:
        if category != previous:
            guide += [f"## {category}", ""]
            previous = category
        guide += [f"### {title}", "", body, ""]
    return {PUBLIC / "USER_GUIDE.md": "\n".join(guide), PUBLIC / "LICENSE": (ROOT / "LICENSE").read_text(encoding="utf-8")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    drift = []
    for path, content in outputs().items():
        if not path.exists() or path.read_text(encoding="utf-8") != content:
            drift.append(path)
            if not args.check:
                path.write_text(content, encoding="utf-8")
    if args.check and drift:
        print("Public guide differs from in-app help; run python -m tools.light_public_docs")
        return 1
    print("Light public guide and licence synchronized")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
