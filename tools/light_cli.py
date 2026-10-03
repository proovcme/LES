"""Small CLI sharing the running LES RAG instance and its canonical API."""
from __future__ import annotations
import argparse
import json
import sys
from tools import light_mcp_server as gateway


def main(argv=None):
    parser = argparse.ArgumentParser(prog='les', description='ЛЕС: чат и поиск в вашей базе знаний')
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('status', 'datasets', 'projects', 'mcp'):
        commands.add_parser(name)
    search = commands.add_parser('search', help='Найти документы с точными фрагментами и ссылками')
    search.add_argument('query')
    search.add_argument('--dataset', action='append', default=[])
    ask = commands.add_parser('ask', help='Задать вопрос назначенной модели Леса')
    ask.add_argument('question')
    ask.add_argument('--chat', help='Продолжить существующий чат по его ID')
    ask.add_argument('--dataset', action='append', default=[])
    args = parser.parse_args(argv)
    if hasattr(sys.stdout, 'reconfigure'): sys.stdout.reconfigure(encoding='utf-8')
    try:
        if args.command == 'mcp':
            gateway.main()
            return 0
        if args.command == 'search':
            result = gateway.search_sources(args.query, args.dataset)
        elif args.command == 'datasets':
            result = gateway.list_datasets()
        elif args.command == 'ask':
            sid = args.chat or gateway._request('POST', '/api/workspace/sessions', body={'title': args.question[:80]})['session_id']
            result = gateway._request('POST', '/api/chat', body={
                'session_id': sid, 'question': args.question, 'dataset_ids': args.dataset,
                'mode': 'search' if args.dataset else 'agent',
            }, timeout=240)
            result = dict(result, session_id=sid)
        else:
            result = gateway._request('GET', '/api/status' if args.command == 'status' else '/api/projects')
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (RuntimeError, ValueError, KeyError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
