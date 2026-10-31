import DOMPurify from 'dompurify';
import { marked } from 'marked';
import type { Source } from './contracts';

/** Only references present in the backend source map become evidence buttons. */
export function answerHTML(text: string, sources: Source[]): string {
  const clean = DOMPurify.sanitize(marked.parse(text, { async: false, breaks: true }), {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ['img', 'style', 'iframe', 'form', 'input', 'button', 'select', 'textarea', 'option', 'label', 'video', 'audio', 'object', 'embed'],
    FORBID_ATTR: ['style', 'id', 'name', 'target'], ALLOW_DATA_ATTR: false,
  });
  const container = document.createElement('div');
  container.innerHTML = clean;
  for (const anchor of container.querySelectorAll('a')) {
    try {
      const url = new URL(anchor.getAttribute('href') || '');
      if (!['https:', 'http:'].includes(url.protocol)) throw new Error();
      const host = url.hostname.toLowerCase();
      if (url.origin === location.origin || !host.includes('.') || host.endsWith('.localhost') || host.endsWith('.local')
        || /^(127\.|0\.|10\.|192\.168\.|169\.254\.|172\.(1[6-9]|2\d|3[01])\.)/.test(host)) throw new Error();
      anchor.target = '_blank'; anchor.rel = 'noopener noreferrer';
    } catch { anchor.removeAttribute('href'); }
  }
  const walker = document.createTreeWalker(container, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  while (walker.nextNode()) {
    const node = walker.currentNode as Text;
    if (!node.parentElement?.closest('a,pre,code')) nodes.push(node);
  }
  for (const node of nodes) {
    const matches = [...node.data.matchAll(/\[(?:Источник\s+)?(\d+)\]/gi)];
    if (!matches.length) continue;
    const fragment = document.createDocumentFragment(); let cursor = 0;
    for (const match of matches) {
      const prefix = node.data.slice(cursor, match.index);
      const source = sources.find((item) => item.index === Number(match[1]));
      if (source) {
        const button = document.createElement('button');
        button.type = 'button'; button.className = 'citation'; button.dataset.citation = String(source.index);
        button.textContent = `[${source.index}]`; button.setAttribute('aria-label', `Источник ${source.index}: ${source.doc_name || 'Документ'}`);
        const word = /\S+\s*$/.exec(prefix);
        if (word && word[0].trim().length <= 40) {
          fragment.append(prefix.slice(0, word.index));
          const anchor = document.createElement('span'); anchor.className = 'citation-anchor';
          anchor.append(prefix.slice(word.index), button); fragment.append(anchor);
        } else fragment.append(prefix, button);
      } else fragment.append(prefix, match[0]);
      cursor = match.index + match[0].length;
    }
    fragment.append(node.data.slice(cursor)); node.replaceWith(fragment);
  }
  return container.innerHTML;
}

export function citations(node: HTMLElement, open: (index: number) => void) {
  const click = (event: MouseEvent) => {
    const button = (event.target as Element)?.closest<HTMLButtonElement>('button[data-citation]');
    if (button) open(Number(button.dataset.citation));
  };
  node.addEventListener('click', click);
  return { update(value: typeof open) { open = value; }, destroy() { node.removeEventListener('click', click); } };
}
