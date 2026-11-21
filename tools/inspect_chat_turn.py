"""Read one native chat turn by history_id without exporting prompts or documents."""
import argparse
from contextlib import closing
import json
from pathlib import Path
import sqlite3


def fields(value, names):
    return {name: value[name] for name in names if name in value}


def inspect(database, history_id):
    with closing(sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True)) as db:
        row = db.execute(
            'SELECT resolved_dataset_ids, success, retrieval_trace_json, artifact_json '
            'FROM chat_history WHERE id=?', (history_id,),
        ).fetchone()
    if row is None:
        raise ValueError(f'Unknown history_id {history_id}')
    datasets, success, trace_json, artifact_json = row
    trace = json.loads(trace_json or '{}')
    artifact = json.loads(artifact_json or '{}')
    search = trace.get('prefetch') or {}
    identity = ('index', 'dataset_id', 'doc_id', 'source_sha256', 'qdrant_point_id',
                'reading_unit_sha256', 'reading_representation', 'page', 'row', 'verified_cells')
    calls = []
    for attempt in trace.get('contexts', []):
        window = attempt.get('context') or {}
        calls.append({
            **fields(attempt, ('table_selection',)),
            **fields(window, ('delivery', 'sent_to_model', 'input_budget_tokens', 'included_tokens')),
            'sources': [fields(s, identity) for s in attempt.get('sources', [])],
            'omissions': [fields(s, ('kind', 'reason', 'total', 'omitted')) for s in window.get('omissions', [])],
        })
    facts = artifact.get('table_facts') or {}
    return {
        'history_id': history_id, 'success': bool(success), 'datasets': json.loads(datasets or '[]'),
        **fields(trace, ('mode', 'status', 'cell_selection_retry', 'citation_retry', 'grounding_retry')),
        'retrieval': {**fields(search, ('index_revision', 'candidates', 'coverage', 'fusion')),
                      'point_ids': (search.get('read_receipt') or {}).get('point_ids', []),
                      'documents': [fields(s, identity) for s in search.get('candidate_documents', [])]},
        'calls': calls, 'citations': fields(trace.get('citation_check') or {}, ('status', 'linked', 'unresolved')),
        'outcome': fields(artifact, ('completion_status', 'validation')),
        'error': (artifact.get('error') or {}).get('code'),
        'selected_rows': [fields(s, ('doc_id', 'source_sha256', 'source_index', 'row', 'kind'))
                          for s in facts.get('rows', [])],
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database', type=Path)
    parser.add_argument('history_id', type=int)
    args = parser.parse_args()
    try:
        print(json.dumps(inspect(args.database, args.history_id), ensure_ascii=False, indent=2))
    except (ValueError, sqlite3.Error) as exc:
        parser.exit(1, str(exc) + '\n')
