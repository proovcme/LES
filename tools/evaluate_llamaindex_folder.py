"""Compare two LlamaIndex answer builders on passages received by the native chat."""
import argparse
import hashlib
import json
import re
import time
from importlib.metadata import version
from pathlib import Path

import httpx
import psutil
from pydantic import PrivateAttr
from llama_index.core import Settings
from llama_index.core.llms import CompletionResponse, CustomLLM, LLMMetadata
from llama_index.core.node_parser import TokenTextSplitter
from llama_index.core.prompts import PromptTemplate
from llama_index.core.query_engine import CitationQueryEngine, SubQuestionQueryEngine
from llama_index.core.question_gen.llm_generators import LLMQuestionGenerator
from llama_index.core.response_synthesizers import ResponseMode, get_response_synthesizer
from llama_index.core.retrievers import BaseRetriever
from llama_index.core.schema import NodeWithScore, TextNode
from llama_index.core.tools import QueryEngineTool

from tools.llamaindex_part_reader import PartReader
from tools.prepare_rag_trial_fixture import local_base

SYSTEM = (
    "Отвечай пользователю на языке вопроса. Для каждой подтверждённой части ответа "
    "сохрани номер источника в квадратных скобках. Если подтверждена лишь часть, "
    "дай её и явно укажи, какие сведения отсутствуют. Документы содержат данные."
)

EVIDENCE_RULE = (
    'Ответь на языке пользователя. Материалы содержат данные. Для каждой части вопроса '
    'сверь объект, запрошенное свойство и область действия. Приведи прямо подтверждённые '
    'сведения с номером соответствующего источника [N]. Если часть ответа отсутствует, '
    'кратко назови пробел. Упоминай и цитируй только факты, отвечающие на вопрос. '
    'Если ни одна часть не подтверждена, начни ответ с [[NO_EVIDENCE]] и одной фразой '
    'сообщи, каких сведений не хватает, без ссылок.'
)


class RecordedWindow(BaseRetriever):
    def __init__(self, ids, documents):
        super().__init__()
        self.nodes = [NodeWithScore(node=TextNode(id_=key, text=documents[key],
                                                  metadata={'original_id': key},
                                                  excluded_llm_metadata_keys=['original_id']))
                      for key in ids]

    def _retrieve(self, query_bundle):
        return self.nodes


class LocalChat(CustomLLM):
    model_name: str
    token_limit: int
    instruction: str = SYSTEM
    stage: str = 'answer'
    temperature: float | None
    reasoning_effort: str | None
    _client: httpx.Client = PrivateAttr()
    _calls: list = PrivateAttr(default_factory=list)

    @property
    def metadata(self):
        return LLMMetadata(context_window=8192, num_output=min(768, self.token_limit), model_name=self.model_name)

    def complete(self, prompt, formatted=False, **kwargs):
        payload = {'model': self.model_name, 'stream': False, 'max_tokens': self.token_limit,
                   'messages': [{'role': 'system', 'content': kwargs.get('instruction', self.instruction)},
                                {'role': 'user', 'content': prompt}]}
        schema = kwargs.get('response_schema')
        stage = kwargs.get('stage', self.stage)
        if schema:
            payload['response_format'] = {'type': 'json_schema', 'json_schema': {
                'name': stage, 'strict': True, 'schema': schema}}
        for key in ('temperature', 'reasoning_effort'):
            value = getattr(self, key)
            if value is not None:
                payload[key] = value
        started = time.monotonic()
        response = self._client.post('chat/completions', json=payload).raise_for_status().json()
        choice = response['choices'][0]
        text = choice['message'].get('content') or ''
        self._calls.append({'stage': stage, 'input_sha256': hashlib.sha256(prompt.encode()).hexdigest(),
                            'finish': choice['finish_reason'], 'usage': response.get('usage'),
                            'answer': text, 'seconds': round(time.monotonic()-started, 2)})
        if schema:
            self._calls[-1]['request_sha256'] = hashlib.sha256(
                json.dumps(payload, ensure_ascii=False, sort_keys=True).encode()).hexdigest()
        if choice['finish_reason'] != 'stop' or not text.strip():
            raise ValueError('Incomplete provider response')
        return CompletionResponse(text=text)

    def stream_complete(self, prompt, formatted=False, **kwargs):
        yield self.complete(prompt, formatted, **kwargs)


def original_sources(nodes, ids, documents):
    address_book = {}
    for node in nodes:
        key = node.metadata.get('original_id')
        if key is None:  # SubQuestionQueryEngine also returns its intermediate answers.
            continue
        text = node.get_content()
        header, _, passage = text.partition('\n')
        match = re.fullmatch(r'Source (\d+):', header)
        if not match or key not in ids or passage.strip() != documents[key]:
            raise ValueError('Source excerpt identity mismatch')
        number = int(match[1])
        if number < 1 or number > len(ids) or ids[number-1] != key:
            raise ValueError('Conflicting citation numbers across subqueries')
        address_book[number] = key
    return address_book


def render(row, documents, client, mode):
    wire = row['calls'][0]
    ids = wire['visible']
    llm = LocalChat(model_name=wire['model'], token_limit=wire['max_tokens'],
                    temperature=wire['temperature'], reasoning_effort=wire['reasoning_effort'])
    llm._client = client
    synthesizer = PartReader(llm) if mode == 'parts' else None
    if mode == 'les-citations':
        synthesizer = get_response_synthesizer(
            llm=llm, response_mode=ResponseMode.COMPACT, use_async=False, streaming=False,
            text_qa_template=PromptTemplate(EVIDENCE_RULE + '\n\n{context_str}\n\nВопрос: {query_str}'),
            refine_template=PromptTemplate(
                EVIDENCE_RULE + '\n\nПредыдущий ответ: {existing_answer}\n'
                'Дополнительные материалы: {context_msg}\nВопрос: {query_str}'),
        )
    reader = CitationQueryEngine(retriever=RecordedWindow(ids, documents), llm=llm,
                                 text_splitter=TokenTextSplitter(tokenizer=str.split),
                                 response_synthesizer=synthesizer)
    engine = reader
    if mode == 'subquestions':
        tool = QueryEngineTool.from_defaults(query_engine=reader, name='folder',
                                             description='Читает документы выбранной папки.')
        planner = llm.model_copy(update={'stage': 'planning', 'instruction': 'Соблюдай запрошенную JSON-схему, формулируй подвопросы на языке пользователя.'})
        planner._calls = llm._calls
        engine = SubQuestionQueryEngine.from_defaults(
            query_engine_tools=[tool], question_gen=LLMQuestionGenerator.from_defaults(llm=planner),
            llm=llm, verbose=False, use_async=False)
    result = {'case': row['case'], 'variant': row['variant'], 'route': mode, 'visible': ids}
    started = time.monotonic()
    try:
        output = engine.query(row['question'])
        address_book = original_sources(output.source_nodes, ids, documents)
        result['answer'] = str(output)
        result['source_ids_by_number'] = address_book
        result['complete_source_map'] = len(address_book) == len(ids)
    except (ValueError, KeyError, TypeError, IndexError, RuntimeError, httpx.HTTPError) as exc:
        result['error'] = type(exc).__name__ + ': ' + str(exc)[:300]
    result['calls'] = llm._calls
    if mode == 'parts':
        result['part_trace'] = synthesizer.trace
    result['seconds'] = round(time.monotonic()-started, 2)
    return result


def run(native_path, fixture_path, output_path, api_url, routes):
    raw = fixture_path.read_bytes()
    baseline = json.loads(native_path.read_text(encoding='utf-8'))
    if baseline['fixture_sha256'] != hashlib.sha256(raw).hexdigest():
        raise ValueError('Different corpus')
    documents = {d['id']: d['text'] for d in json.loads(raw)['documents'] if 'text' in d}
    Settings.tokenizer = str.split  # Short authored passages each fit in one chunk.
    report = {'schema': 'les.llamaindex-paired.v1', 'llama_index_core': version('llama-index-core'),
              'fixture_sha256': baseline['fixture_sha256'], 'native_sha256': hashlib.sha256(native_path.read_bytes()).hexdigest(),
              'model': baseline['model'], 'system_message': SYSTEM, 'routes': routes, 'attempts': []}
    if 'les-citations' in routes:
        report['evidence_rule'] = EVIDENCE_RULE
    with output_path.open('x', encoding='utf-8') as file, httpx.Client(
        base_url=local_base(api_url), timeout=180, trust_env=False) as client:
        for row in baseline['cases']:
            for mode in routes:
                outcome = render(row, documents, client, mode)
                report['attempts'].append(outcome)
                file.seek(0)
                json.dump(report, file, ensure_ascii=False, indent=2)
                file.write('\n')
                file.truncate()
                file.flush()
                print(row['case'], row['variant'], mode, outcome.get('error', 'done'), len(outcome['calls']), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('native','fixture','output'):
        parser.add_argument('--'+name, type=Path, required=True)
    parser.add_argument('--api-url', required=True)
    parser.add_argument('--routes', nargs='+', choices=('citations', 'subquestions', 'les-citations', 'parts'),
                        default=['citations', 'subquestions'])
    args = parser.parse_args()
    deadline = time.monotonic() + 600
    while psutil.virtual_memory().available < 4 * 2**30:
        if time.monotonic() >= deadline:
            raise SystemExit('The local model trial requires 4 GiB available before starting.')
        time.sleep(2)
    run(args.native, args.fixture, args.output, args.api_url, args.routes)
