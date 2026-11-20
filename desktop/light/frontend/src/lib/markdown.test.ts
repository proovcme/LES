import { expect, it } from 'vitest';
import { answerHTML, citations } from './markdown';

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

it('repairs only exact supplied source headers in saved answers, outside code and links', () => {
  const sources = [{index:1,doc_name:'файл 87.pdf',header:'[Источник 1 | файл 87.pdf | TABLE]'}];
  const div = document.createElement('div');
  div.innerHTML = answerHTML('*Источник 1 | файл 87.pdf | TABLE]* [Источник 1 | файл 87.pdf | TABLE] `Источник 1 | файл 87.pdf | TABLE]` Источник 1. [999]', sources);
  expect([...div.querySelectorAll('button')].map((b) => b.dataset.citation)).toEqual(['1','1']);
  expect(div.querySelector('code')?.textContent).toContain('файл 87.pdf');
  expect(div.textContent).toContain('Источник 1. [999]');
});

it('preserves a quoted row number and shows the verified address on the citation', () => {
  const div = document.createElement('div');
  div.innerHTML = answerHTML('- Сигнализатор (лист **«ПБ»**, строка **8**, позиция 13) [Источник 21].\n- Блок (лист СО_ДИС, строка 8) [Источник 18].', [
    { index: 21, row: 99, sheet_name: 'старый', locator: { sheet: 'ПБ', row: 18 } },
    { index: 18, sheet_name: 'СО_ДИС', row: 8 },
  ]);
  expect(div.querySelector('li')?.textContent).toContain('лист «ПБ», строка 8, позиция 13');
  expect(div.querySelector('[data-citation="21"]')?.getAttribute('aria-label')).toContain('ПБ · строка 18');
  expect(div.querySelectorAll('li')[1].textContent).toContain('СО_ДИС, строка 8');
});

it('leaves ambiguous, unmatched, uncited, range and code references alone', () => {
  const div = document.createElement('div');
  div.innerHTML = answerHTML('- лист ПБ, строка 8 [1] [2]\n- лист ПБ, строка 8 [999]\n- лист ПБ, строка 8–10 [1]\n- лист другой, строка 9 [1]\n- `лист ПБ, строка 8` [1]', [
    { index: 1, sheet_name: 'ПБ', row: 18 }, { index: 2, sheet_name: 'другой', row: 20 },
  ]);
  expect(div.textContent).not.toContain('строка 18');
  expect(div.querySelector('code')?.textContent).toBe('лист ПБ, строка 8');
});

it('opens the cited row while preserving the statement about a different empty row', () => {
  const div = document.createElement('div');
  const opened: number[] = [];
  const handler = citations(div, index => opened.push(index));
  div.innerHTML = answerHTML('Запрошенный адрес (лист Register, строка 8) пуст. Значение найдено ниже [Источник 1].', [
    { index: 1, locator: { sheet: 'Register', row: 18 } },
  ]);
  const button = div.querySelector('button')!;
  expect(div.textContent).toContain('(лист Register, строка 8) пуст.');
  expect(button.title).toContain('Register · строка 18');
  button.click();
  expect(opened).toEqual([1]);
  handler.destroy();
});
