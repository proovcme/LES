import { expect, it } from 'vitest';
import { answerHTML } from './markdown';

it('makes only backend-mapped citations clickable and leaves code untouched', () => {
  const div = document.createElement('div');
  div.innerHTML = answerHTML('Ответ [1], [Источник 2], [999]. `код [1]`', [{ index: 1, doc_name: 'СП пример.pdf' }, { index: 2, doc_name: 'Условия.docx' }]);
  expect([...div.querySelectorAll('button')].map((button) => button.dataset.citation)).toEqual(['1', '2']);
  expect(div.querySelector('code')?.textContent).toBe('код [1]');
  expect(div.textContent).toContain('[999]');
});

it('rejects scripts, HTML event handlers, fake evidence buttons and local file links', () => {
  const div = document.createElement('div');
  div.innerHTML = answerHTML('<script>alert(1)</script><img src=x onerror=alert(1)><button data-citation="9">Fake</button>\n\n[bad](javascript:alert) [disk](file:///C:/private) [api](/api/documents/by-id/forged/raw) [local](http://127.0.0.1:8090/api/private) [alias](http://2130706433:8090/api/private) [web](https://example.org)', []);
  expect(div.querySelector('script,img,button,[onerror]')).toBeNull();
  expect([...div.querySelectorAll('a[href]')].map((link) => link.getAttribute('href'))).toEqual(['https://example.org']);
  expect(div.querySelector('a[href]')?.getAttribute('rel')).toBe('noopener noreferrer');
});
