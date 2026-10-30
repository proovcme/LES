"""Generate answers to saved trial packets; semantic grades require separate review."""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path: sys.path.insert(0, str(ROOT))
from tools.rag_trial_model import TrialOllama


def evaluate(trial, fixture_path, output, *, api_url, model, per_category=4, output_tokens=1200):
    fixture = json.loads(fixture_path.read_text(encoding='utf-8'))
    counts, selected = Counter(), set()
    for case in fixture['cases']:
        if per_category <= 0 or counts[case['category']] < per_category:
            counts[case['category']] += 1
            selected.add(case['id'])
    report = json.loads((trial/'report.json').read_text(encoding='utf-8'))
    if report['input_tokens'] != 8192 or not 1 <= output_tokens <= 1200:
        raise ValueError('This model trial supports 8192 context and up to the reserved 1200 output tokens')
    cases = {case['id']: (index, case) for index, case in enumerate(fixture['cases'])}
    buckets = {}
    for case in fixture['cases']:
        if case['id'] in selected:
            buckets.setdefault(case['category'], []).append(case['id'])
    # Round-robin categories: an early resource stop must not sample only easy facts.
    case_order = [bucket[index] for index in range(max(map(len, buckets.values()), default=0))
                  for bucket in buckets.values() if index < len(bucket)]
    runs = [run for case_id in case_order for run in report['runs'] if run['case_id'] == case_id]
    output.mkdir(parents=True, exist_ok=False)
    model_client = TrialOllama(api_url, model)
    result = {'schema': 'les.rag-trial-answers.v1', 'model': model,
              'output_tokens': output_tokens,
              'scope': 'saved production-governed packets; no UI/tool-loop/ingestion acceptance',
              'fixture_sha256': hashlib.sha256(fixture_path.read_bytes()).hexdigest(),
              'trial_report_sha256': hashlib.sha256((trial/'report.json').read_bytes()).hexdigest(),
              'semantic_accuracy': None, 'cases': []}
    try:
        for run in runs:
            if run['case_id'] not in selected: continue
            index, case = cases[run['case_id']]
            row = {'case_id': case['id'], 'category': case['category'], 'profile': run['profile'],
                   'rrf_constant': run['rrf_constant'], 'expected': case['expected']}
            if run.get('error'):
                row['error'] = 'TRIAL_PACKET_FAILED'
                result['cases'].append(row)
                continue
            packet = json.loads((trial/'evidence'/f"{index:04d}-{run['profile']}-{run['rrf_constant']:g}.json").read_text(encoding='utf-8'))
            if packet.get('case_id') != case['id'] or packet.get('question') != case['question']:
                raise ValueError('Saved packet and authored case differ; regenerate the trial')
            try:
                row.update(model_client.ask(packet['messages'], output_tokens=output_tokens))
                row['generation_complete'] = row.get('done_reason') != 'length'
                row['sources'] = packet['sources']
            except Exception as error:
                row['error'] = str(error)
                result['cases'].append(row)
                result['stopped'] = type(error).__name__
                break  # No retry loop through admission errors or a failed model.
            result['cases'].append(row)
            print(f"answer {len(result['cases'])}: {case['id']} {run['profile']}", flush=True)
            (output/'answers.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    finally:
        model_client.close()
        (output/'answers.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('trial', 'fixture', 'output'): parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--api-url', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--per-category', type=int, default=4)
    parser.add_argument('--output-tokens', type=int, default=1200)
    args = parser.parse_args()
    report = evaluate(args.trial, args.fixture, args.output, api_url=args.api_url, model=args.model,
                      per_category=args.per_category, output_tokens=args.output_tokens)
    print(json.dumps({'responses': len(report['cases']), 'stopped': report.get('stopped'), 'semantic_accuracy': None}))


if __name__ == '__main__': main()
