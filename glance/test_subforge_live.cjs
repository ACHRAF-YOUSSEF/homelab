const assert = require('node:assert/strict');
const test = require('node:test');

const client = require('./config/assets/subforge-live.js');

class FakeNode {
  constructor(tag, ownerDocument) {
    this.tagName = tag;
    this.ownerDocument = ownerDocument;
    this.children = [];
    this.parentElement = null;
    this.dataset = {};
    this.attributes = {};
    this.listeners = {};
    this.style = {};
    this.className = '';
    this.textContent = '';
    this.hidden = false;
    this.title = '';
  }
  append(...nodes) {
    for (const node of nodes) {
      node.remove();
      node.parentElement = this;
      this.children.push(node);
    }
  }
  replaceChildren(...nodes) {
    for (const child of this.children) child.parentElement = null;
    this.children = [];
    this.append(...nodes);
  }
  remove() {
    if (!this.parentElement) return;
    this.parentElement.children = this.parentElement.children.filter(child => child !== this);
    this.parentElement = null;
  }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  querySelector(selector) {
    if (selector === '[data-subforge-link]') {
      return this.children.find(child => child.attributes['data-subforge-link'] !== undefined)
        || this.children.map(child => child.querySelector(selector)).find(Boolean) || null;
    }
    return null;
  }
  querySelectorAll(selector) {
    const match = selector === 'article' ? node => node.tagName === 'article' : () => false;
    return this.children.flatMap(child => [...(match(child) ? [child] : []), ...child.querySelectorAll(selector)]);
  }
  isConnected() {
    for (let node = this; node; node = node.parentElement) if (node.connectedRoot) return true;
    return false;
  }
}

function fixture() {
  const doc = {
    hidden: false,
    listeners: {},
    createElement(tag) { return new FakeNode(tag, doc); },
    addEventListener(name, callback) { this.listeners[name] = callback; },
  };
  const host = {
    listeners: {},
    addEventListener(name, callback) { this.listeners[name] = callback; },
  };
  doc.defaultView = host;
  const content = new FakeNode('div', doc);
  content.connectedRoot = true;
  content.isConnected = true;
  const publicLink = new FakeNode('a', doc);
  publicLink.setAttribute('data-subforge-link', '');
  publicLink.setAttribute('href', '/subforge/');
  content.append(publicLink);

  const originalEventSource = global.EventSource;
  const originalSetInterval = global.setInterval;
  const originalClearInterval = global.clearInterval;
  const originalSetTimeout = global.setTimeout;
  const originalClearTimeout = global.clearTimeout;
  const originalPerformance = global.performance;
  const streams = [];
  const cleared = [];
  const retries = new Map();
  let nextRetry = 1;
  let now = 1000;
  class FakeEventSource {
    constructor(url) { this.url = url; this.listeners = {}; this.closeCount = 0; streams.push(this); }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    close() { this.closeCount++; }
    emit(name, data) { this.listeners[name]?.({ data: JSON.stringify(data) }); }
  }
  global.EventSource = FakeEventSource;
  global.setInterval = () => streams.length;
  global.clearInterval = id => cleared.push(id);
  global.setTimeout = (callback, delay) => { const id = nextRetry++; retries.set(id, { callback, delay }); return id; };
  global.clearTimeout = id => retries.delete(id);
  global.performance = { now: () => now };
  const mounted = client.mount(content, 'https://glance.example.test/gateway');
  const byId = id => content.querySelectorAll('article').find(row => row.dataset.jobId === id);
  const allText = node => [node.textContent, ...node.children.map(allText)].filter(Boolean).join(' ');
  return {
    doc, host, content, mounted, streams, cleared, retries,
    setNow(value) { now = value; },
    byId,
    rows: () => content.querySelectorAll('article'),
    allText,
    status: () => content.children.find(node => node.className.includes('glance-live-status')),
    restore() {
      global.EventSource = originalEventSource;
      global.setInterval = originalSetInterval;
      global.clearInterval = originalClearInterval;
      global.setTimeout = originalSetTimeout;
      global.clearTimeout = originalClearTimeout;
      global.performance = originalPerformance;
    },
  };
}

const job = (id, options = {}) => ({
  id, kind: 'video', status: 'running', liveness: 'running', phase: 'ocr',
  processed: 2, total: 10, progressPercent: 20, elapsedSeconds: 30, etaSeconds: 90,
  waitReason: '', ...options,
});
const data = (jobs = []) => ({
  convertedCount: 4, queuedCount: 2, configuredRootCount: 3, recentFailuresCount: 1,
  jobs, selectedLanguages: ['en', 'fr'],
});
const snapshot = (jobs = [], options = {}) => ({ status: 'ok', data: data(jobs), updatedAt: '2026-10-17T10:00:00Z', ...options });
const send = (page, message) => page.streams.at(-1).emit('subforge', message);

test('terminal stream errors retry with bounded backoff and lifecycle cleanup', t => {
  const page = fixture();
  t.after(() => page.restore());
  const first = page.streams[0];
  first.readyState = 0;
  first.onerror();
  assert.equal(page.retries.size, 0, 'ordinary native reconnect stays in charge');
  for (const delay of [1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
    const current = page.streams.at(-1);
    current.readyState = 2;
    current.onerror();
    assert.equal(page.retries.size, 1);
    const [id, retry] = [...page.retries][0];
    assert.equal(retry.delay, delay);
    page.retries.delete(id);
    retry.callback();
  }
  send(page, snapshot([job('live-job')]));
  first.emit('subforge', snapshot([]));
  assert.equal(page.rows().length, 1, 'closed stream cannot overwrite current jobs');
  page.streams.at(-1).readyState = 2;
  page.streams.at(-1).onerror();
  assert.equal([...page.retries.values()][0].delay, 1000, 'healthy data resets backoff');
  page.host.listeners.pagehide();
  assert.equal(page.retries.size, 0);
  const count = page.streams.length;
  page.doc.listeners.visibilitychange();
  assert.equal(page.streams.length, count, 'pagehide prevents premature reopen');
  page.host.listeners.pageshow();
  assert.equal(page.streams.length, count + 1);
  page.mounted.close();
});

test('jobs keep independent progress and timing labels, keyed by stable IDs', t => {
  const page = fixture();
  t.after(() => page.restore());
  send(page, snapshot([
    job('job-a-001', { processed: 2, progressPercent: 20, elapsedSeconds: 35, etaSeconds: 65 }),
    job('job-b-002', { processed: 8, progressPercent: 80, elapsedSeconds: 100, etaSeconds: 20, liveness: 'waiting', waitReason: 'Waiting for worker' }),
  ]));

  assert.equal(page.rows().length, 2);
  assert.equal(page.byId('job-a-001').children[1].textContent, 'ocr · 2/10 files');
  assert.equal(page.byId('job-b-002').children[1].textContent, 'ocr · 8/10 files');
  assert.equal(page.byId('job-a-001').children[2].children[0].style.width, '20%');
  assert.equal(page.byId('job-b-002').children[2].children[0].style.width, '80%');
  assert.match(page.byId('job-a-001').children[3].textContent, /Elapsed 35s · Approx\. 1m 5s remaining/);
  assert.match(page.byId('job-b-002').children[3].textContent, /Elapsed 1m 40s · ETA unavailable/);
  assert.equal(page.byId('job-b-002').children[4].textContent, 'Waiting for worker');
  assert.equal(page.status().textContent, 'Live');
});

test('stable rows are reused across snapshots and removed when jobs disappear', t => {
  const page = fixture();
  t.after(() => page.restore());
  send(page, snapshot([job('keep-001'), job('remove-002')]));
  const retained = page.byId('keep-001');
  const removed = page.byId('remove-002');
  send(page, snapshot([job('keep-001', { processed: 7, progressPercent: 70 })]));

  assert.equal(page.byId('keep-001'), retained);
  assert.equal(page.byId('keep-001').children[1].textContent, 'ocr · 7/10 files');
  assert.equal(page.byId('remove-002'), undefined);
  assert.equal(removed.parentElement, null);
  assert.equal(page.rows().length, 1);
});

test('unknown ETA stays unavailable and does not invent progress', t => {
  const page = fixture();
  t.after(() => page.restore());
  send(page, snapshot([job('unknown-eta', { etaSeconds: null, progressPercent: null, total: 0 })]));
  const row = page.byId('unknown-eta');
  assert.match(row.children[3].textContent, /ETA unavailable/);
  assert.equal(row.children[2].hidden, true);
  assert.equal(row.children[2].children[0].style.width, undefined);
});

test('timing interpolates from the snapshot for at most 15 seconds and then freezes', t => {
  const page = fixture();
  t.after(() => page.restore());
  send(page, snapshot([job('timed-001', { elapsedSeconds: 10, etaSeconds: 30 })]));
  const row = page.byId('timed-001');
  page.setNow(4000);
  page.mounted.tick();
  assert.match(row.children[3].textContent, /Elapsed 13s · Approx\. 27s remaining/);
  page.setNow(15999);
  page.mounted.tick();
  const frozen = row.children[3].textContent;
  page.setNow(16001);
  page.mounted.tick();
  assert.equal(row.children[3].textContent, frozen);
  assert.equal(page.status().textContent, 'Live');
  page.setNow(31000);
  page.mounted.tick();
  assert.equal(page.status().textContent, 'Live');
  assert.equal(row.children[3].textContent, frozen);
  page.setNow(41001);
  page.mounted.tick();
  assert.match(page.status().textContent, /Updates delayed/);
});

test('stale snapshots and malformed snapshots retain the last valid rows', t => {
  const page = fixture();
  t.after(() => page.restore());
  send(page, snapshot([job('last-good-001')]));
  const retained = page.byId('last-good-001');
  send(page, { status: 'stale', data: null, updatedAt: '2026-10-17T10:01:00Z' });
  assert.equal(page.byId('last-good-001'), retained);
  assert.match(page.status().textContent, /showing last data/);
  send(page, { status: 'ok', data: { ...data([job('invalid')]), queuedCount: 'not-a-number' } });
  assert.equal(page.byId('last-good-001'), retained);
  assert.equal(page.byId('invalid'), undefined);
  assert.match(page.status().textContent, /Updates unavailable/);
});

test('only a valid empty snapshot shows the no-jobs message', t => {
  const page = fixture();
  t.after(() => page.restore());
  assert.equal(page.content.children.length, 2); // public link and connection status
  send(page, { status: 'warming', data: null });
  assert.equal(page.content.children.some(node => node.textContent === 'No jobs running.'), false);
  send(page, snapshot([]));
  const empty = page.content.children.find(node => node.textContent === 'No jobs running.');
  assert.ok(empty);
  assert.equal(empty.hidden, false);
});

test('hidden and page lifecycle events close the stream and reconnect without duplicates', t => {
  const page = fixture();
  t.after(() => page.restore());
  const original = page.streams[0];
  assert.equal(page.streams.length, 1);
  page.mounted.connect();
  assert.equal(page.streams.length, 1);

  page.doc.hidden = true;
  page.doc.listeners.visibilitychange();
  assert.equal(original.closeCount, 1);
  assert.equal(page.status().textContent, 'Updates paused');
  page.host.listeners.pagehide();
  assert.equal(original.closeCount, 1);

  page.doc.hidden = false;
  page.host.listeners.pageshow();
  assert.equal(page.streams.length, 2);
  page.host.listeners.pageshow();
  page.mounted.connect();
  assert.equal(page.streams.length, 2);
});

test('EventSource uses only the public gateway route and has no credential query', t => {
  const page = fixture();
  t.after(() => page.restore());
  const url = new URL(page.streams[0].url);
  assert.equal(url.origin, 'https://glance.example.test');
  assert.equal(url.pathname, '/gateway/live/events');
  assert.deepEqual([...url.searchParams.entries()], [['page', 'media'], ['topic', 'subforge']]);
  assert.equal(url.searchParams.has('token'), false);
  assert.equal(url.username, '');
  assert.equal(url.password, '');
});

test('duration formatter rejects unknown values and formats bounded nonnegative seconds', () => {
  assert.equal(client.duration(null), 'unavailable');
  assert.equal(client.duration(-1), 'unavailable');
  assert.equal(client.duration(Number.NaN), 'unavailable');
  assert.equal(client.duration(3661.9), '1h 1m 1s');
});
