"""Read complete indexed parents and pack independently citable fragments.

Candidate only: no production hook, model call or implicit index migration.
Completeness means all indexed fragments, not guaranteed parser completeness.
"""
from dataclasses import dataclass
import json
from typing import Callable


@dataclass(frozen=True)
class Fragment:
    point_id: str
    dataset_id: str
    file_name: str
    parent_id: str
    ordinal: int
    text: str
    pages: tuple

    @property
    def section_key(self):
        return (self.dataset_id, self.file_name, self.parent_id or self.point_id)

    @classmethod
    def from_point(cls, point):
        payload = point.payload or {}
        if payload.get("node_role") == "navigation" or payload.get("evidence_admissible") is False:
            raise ValueError("Navigation is not evidence")
        dataset, name = str(payload.get("dataset_id") or ""), str(payload.get("file_name") or "")
        if not dataset or not name or not str(payload.get("text") or "").strip():
            raise ValueError("Evidence lacks source identity or text")
        pages = payload.get("pages")
        if not pages:
            pages = [next((payload[key] for key in ("page", "source_page", "page_number")
                           if payload.get(key) is not None), None)]
        elif not isinstance(pages, (list, tuple)):
            pages = [pages]
        return cls(str(point.id), dataset, name, str(payload.get("parent_id") or ""),
                   int(payload.get("chunk_ord") or 0), str(payload["text"]),
                   tuple(p for p in pages if p is not None))


def pack_sections(hits: list[Fragment], read_section: Callable, *, max_tokens: int,
                  count_tokens: Callable[[str], int], max_sections: int = 8) -> dict:
    """Budget the entire serialized evidence envelope, including references.

Read each unique parent once. Keep hits first when the complete parent will not
fit. Order accepted fragments by source ordinal, retain retrieval section order.
Never invent a complete section or clip a passage while keeping its old citation.
"""
    if max_tokens <= 0 or max_sections < 1:
        raise ValueError("A positive context budget is required")
    result = {"schema": "les.section-evidence.v1", "trust": "untrusted_source_data", "sections": []}
    render = lambda obj: json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    if count_tokens(render(result)) > max_tokens:
        raise ValueError("Context budget cannot hold the evidence envelope")
    grouped = {}
    for hit in hits:
        grouped.setdefault(hit.section_key, []).append(hit)
    omitted = 0
    pending = []
    for key, seeds in grouped.items():
        if len(result["sections"]) >= max_sections:
            omitted += len(seeds)
            continue
        all_fragments = list(read_section(seeds[0]))
        if any(fragment.section_key != key for fragment in all_fragments):
            raise ValueError("Parent reader crossed a document or dataset boundary")
        by_id = {fragment.point_id: fragment for fragment in all_fragments}
        if len(by_id) != len(all_fragments):
            raise ValueError("Duplicate evidence identity in parent")
        for seed in seeds:
            if by_id.get(seed.point_id) != seed:
                raise ValueError("Index changed or parent no longer contains the search hit")
        ordered = sorted(all_fragments, key=lambda x: (x.ordinal, x.point_id))
        seed_ids = {seed.point_id for seed in seeds}
        section = {"dataset_id": key[0], "file_name": key[1], "parent_id": seeds[0].parent_id,
                   "indexed_fragments": len(ordered), "complete": False, "fragments": []}
        result["sections"].append(section)
        # Reserve retrieved evidence across all sections before adding neighbours.
        # Otherwise a large first parent evicts a later, independently relevant hit.
        for fragment in dict((seed.point_id, seed) for seed in seeds).values():
            item = {"citation_id": fragment.point_id, "ordinal": fragment.ordinal,
                    "pages": list(fragment.pages), "text": fragment.text}
            section["fragments"].append(item)
            section["fragments"].sort(key=lambda x: (x["ordinal"], x["citation_id"]))
            if count_tokens(render(result)) > max_tokens:
                section["fragments"].remove(item)
        if not section["fragments"] or not any(x["citation_id"] in seed_ids for x in section["fragments"]):
            result["sections"].pop()
            omitted += len(seeds)
        else:
            omitted += len(seed_ids - {item['citation_id'] for item in section['fragments']})
            neighbours = sorted((item for item in ordered if item.point_id not in seed_ids),
                key=lambda item: (min(abs(item.ordinal-seed.ordinal) for seed in seeds), item.ordinal, item.point_id))
            pending.append((section, neighbours))
    # Expand nearby source text round-robin, without sacrificing accepted hits.
    for offset in range(max((len(items) for _, items in pending), default=0)):
        for section, neighbours in pending:
            if offset >= len(neighbours):
                continue
            fragment = neighbours[offset]
            item = {"citation_id": fragment.point_id, "ordinal": fragment.ordinal,
                    "pages": list(fragment.pages), "text": fragment.text}
            section['fragments'].append(item)
            section['fragments'].sort(key=lambda x: (x['ordinal'], x['citation_id']))
            if count_tokens(render(result)) > max_tokens:
                section['fragments'].remove(item)
    for section in result['sections']:
        section['complete'] = len(section['fragments']) == section['indexed_fragments']
    # Diagnostics stay outside the token-budgeted model context.
    context = render(result)
    return {"context": context, "tokens": count_tokens(context), "budget": max_tokens,
            "omitted_hits": omitted, "evidence": result,
            "source_map": {item["citation_id"]: {"dataset_id": section["dataset_id"],
                           "file_name": section["file_name"], "pages": item["pages"], "quote": item["text"]}
                           for section in result["sections"] for item in section["fragments"]}}
