import { expect, it } from 'vitest';
import { mount, unmount } from 'svelte';
import Answer from './Answer.svelte';

it('shows unread originals independently of a confident model answer', async () => {
  const component = mount(Answer, { target: document.body, props: {
    message: { role: 'ai', text: 'Полная справка.', meta: { retrieval_trace: { status: 'degraded',
      read_failures: [{ source: { doc_name: 'Расчёт.docx' }, code: 'WORKER_TIMEOUT', message: 'Оригинал не дочитан автоматически.' }] } } },
    openSource: () => {},
  } });
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(document.body.textContent).toContain('Обзор неполный');
  expect(document.body.textContent).toContain('Расчёт.docx');
  expect(document.body.textContent).toContain('Оригинал не дочитан автоматически.');
  await unmount(component); document.body.replaceChildren();
});
