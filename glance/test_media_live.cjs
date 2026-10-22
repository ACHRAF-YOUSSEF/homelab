const assert = require('node:assert/strict');
const test = require('node:test');
const { mount } = require('./config/assets/media-live.js');

class Node {
  constructor(html = '') {
    this.html = html;
    this.children = [];
    this.listeners = {};
    this.className = '';
    this.attributes = {};
    this.textContent = '';
    this.classList = { toggle() {} };
  }
  get innerHTML() { return this.html; }
  set innerHTML(value) { this.html = value; this.children = []; }
  append(...nodes) { this.children.push(...nodes.filter(node => !this.children.includes(node))); }
  setAttribute(key, value) { this.attributes[key] = value; }
  addEventListener(key, fn) { this.listeners[key] = fn; }
  querySelector() { return this.html.includes('unavailable') ? {} : null; }
}

function fixture(initial = 'initial values') {
  const nodes = new Map(['services', 'jellyfin-library', 'automation', 'seerr'].map(name => [`.widget.media-live-${name} .widget-content`, new Node(initial)]));
  const listeners = {}, timers = new Map(), requests = [];
  let nextId = 0;
  const doc = { hidden: false, body: {}, querySelector: selector => nodes.get(selector), createElement: () => new Node(), addEventListener: (name, fn) => { listeners[name] = fn; } };
  const host = {
    AbortController,
    addEventListener: (name, fn) => { listeners[name] = fn; },
    setTimeout: (fn, delay) => { const id = ++nextId; timers.set(id, { fn, delay }); return id; },
    clearTimeout: id => timers.delete(id),
    MutationObserver: class { observe() {} disconnect() {} },
    DOMParser: class { parseFromString(html) { return { querySelector: selector => html === 'missing' ? null : new Node(`${html}:${selector}`) }; } },
    fetch: (url, options) => new Promise((resolve, reject) => { requests.push({ url, options, resolve, reject }); }),
  };
  const client = mount(doc, { baseURL: '/dashboard/' }, host);
  const flush = () => new Promise(resolve => setImmediate(resolve));
  const complete = async (html = 'fresh values') => { requests.at(-1).resolve({ ok: true, text: async () => html }); await flush(); };
  const entry = nodes.values().next().value;
  const footer = entry.children[0];
  return { client, doc, nodes, listeners, timers, requests, flush, complete, entry, footer, message: footer.children[0], time: footer.children[1], retry: footer.children[2] };
}

test('successful checks display time; failures preserve values and time; Retry recovers', async () => {
  const f = fixture();
  assert.equal(f.requests[0].url, '/dashboard/api/pages/media/content/');
  assert.equal(f.message.textContent, 'Checking updates…');
  await f.complete();
  const good = f.entry.innerHTML, checked = f.time.textContent;
  assert.equal(f.message.textContent, 'Up to date');
  assert.match(checked, /^Last checked /);
  assert.match(f.time.title, /not the provider's measurement time/);
  assert.equal(f.retry.hidden, true);
  const failure = f.client.update();
  f.requests.at(-1).reject(Error('private upstream details'));
  await failure;
  assert.equal(f.entry.innerHTML, good);
  assert.equal(f.time.textContent, checked);
  assert.equal(f.message.textContent, 'Refresh failed · showing last data');
  assert.equal(f.retry.hidden, false);
  f.retry.listeners.click();
  assert.equal(f.retry.disabled, true);
  await f.complete('recovered');
  assert.match(f.entry.innerHTML, /^recovered:/);
  assert.equal(f.message.textContent, 'Up to date');
  assert.equal(f.retry.hidden, true);
});

test('HTTP success with missing widgets or provider failure cannot erase last good data', async () => {
  const f = fixture();
  await f.complete();
  const good = f.entry.innerHTML;
  for (const html of ['missing', 'unavailable']) {
    const request = f.client.update();
    await f.complete(html);
    await request;
    assert.equal(f.entry.innerHTML, good);
    assert.equal(f.message.textContent, 'Updates unavailable · showing last data');
    assert.equal(f.retry.hidden, false);
  }
});

test('initial provider failures do not claim any successful refresh', async () => {
  const f = fixture('unavailable');
  await f.complete('unavailable');
  assert.equal(f.message.textContent, 'Service data unavailable');
  assert.equal(f.time.textContent, 'Not checked yet');
  assert.equal(f.retry.hidden, false);
});

test('hidden/pagehide abort requests; late results cannot overwrite resumed checks', async () => {
  const f = fixture();
  const old = f.requests[0];
  f.doc.hidden = true;
  f.listeners.visibilitychange();
  assert.equal(old.options.signal.aborted, true);
  assert.equal(f.message.textContent, 'Updates paused');
  await f.client.update();
  assert.equal(f.requests.length, 1);
  f.doc.hidden = false;
  f.listeners.visibilitychange();
  await f.complete('new values');
  old.resolve({ ok: true, text: async () => 'old values' });
  await f.flush();
  assert.match(f.entry.innerHTML, /^new values:/);
  f.listeners.pagehide();
  assert.equal([...f.timers.values()].filter(timer => timer.delay === 15000).length, 0);
  f.listeners.pageshow();
  await f.complete('restored');
  assert.match(f.entry.innerHTML, /^restored:/);
});

test('manual retries share an in-flight request and timeout aborts a stalled fetch', async () => {
  const f = fixture();
  await f.client.update();
  assert.equal(f.requests.length, 1);
  const deadline = [...f.timers.values()].find(timer => timer.delay === 20000);
  deadline.fn();
  assert.equal(f.requests[0].options.signal.aborted, true);
  f.requests[0].reject(Error('aborted'));
  await f.flush();
  assert.equal(f.retry.disabled, false);
  assert.equal(f.retry.hidden, false);
  assert.equal([...f.timers.values()].filter(timer => timer.delay === 15000).length, 1);
});
