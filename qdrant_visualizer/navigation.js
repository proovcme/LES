/* Shared, bounded, same-origin navigation for the forest and NiceGUI workspaces. */
(() => {
  const home = '/qdrant-visualizer/index.html';
  const key = 'les.navigation.v1';
  const routes = {
    '/classic': ['chat', 'data', 'mail', 'history'],
    '/les/classic': ['state', 'models', 'tools', 'profiles', 'mail'],
  };
  function canonical(value) {
    try {
      const url = new URL(value, location.origin);
      if (url.origin !== location.origin || url.username || url.password) return null;
      if (url.pathname === home) return home;
      const path = url.pathname.replace(/\/$/, '');
      const tabs = routes[path];
      if (!tabs) return null;
      const tab = url.searchParams.get('tab');
      return `${path}?tab=${tabs.includes(tab) ? tab : tabs[0]}`;
    } catch { return null; }
  }
  let trail = [];
  try {
    const saved = JSON.parse(sessionStorage.getItem(key));
    if (Array.isArray(saved)) trail = saved.slice(-40).map(canonical).filter(Boolean);
  } catch { /* A private/locked browser still gets working in-memory navigation. */ }
  function save() {
    trail = trail.slice(-40);
    try { sessionStorage.setItem(key, JSON.stringify(trail)); } catch { /* optional persistence */ }
  }
  function remember(target) {
    if (trail.at(-1) !== target) trail.push(target);
    save();
  }
  function record(value) {
    const target = canonical(value);
    if (!target) return false;
    remember(target);
    if (canonical(location.href) !== target) history.pushState({les: true}, '', target);
    return true;
  }
  function activate(target) {
    const url = new URL(target, location.origin);
    if (url.pathname === location.pathname.replace(/\/$/, '') && typeof window.emitEvent === 'function') {
      history.replaceState({les: true}, '', target);
      window.emitEvent('les-navigation', {path: url.pathname, tab: url.searchParams.get('tab')});
    } else if (canonical(location.href) !== target) location.assign(target);
  }
  function back() {
    const current = canonical(location.href);
    if (trail.at(-1) === current) trail.pop();
    const target = trail.at(-1) || home;
    remember(target);
    activate(target);
  }
  const current = canonical(location.href);
  if (current) remember(current);
  window.addEventListener('popstate', () => {
    const target = canonical(location.href);
    if (!target) return;
    const index = trail.lastIndexOf(target);
    if (index >= 0) trail = trail.slice(0, index + 1);
    else remember(target);
    save();
    activate(target);
  });
  document.addEventListener('click', event => {
    const button = event.target.closest('[data-les-back]');
    if (button) { event.preventDefault(); back(); }
  });
  window.lesNavigation = {record, back, canonical};
})();
