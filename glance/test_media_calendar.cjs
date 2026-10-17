const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const path = require('node:path');
const test = require('node:test');

const calendar = require('./config/assets/media-calendar.js');

const date = value => new Date(`${value}T12:00:00`);
const dayOrdinal = value => {
  const [year, month, day] = value.split('-').map(Number);
  return Date.UTC(year, month - 1, day) / 86400000;
};
const episode = (seriesId, episodeNumber, title = 'Same title', day = '2026-10-20') => ({
  id: `${seriesId}-${episodeNumber}`, slug: `show-${seriesId}`, seriesId, kind: 'episode',
  title, season: 2, episode: episodeNumber, date: day,
});

test('civil-day arithmetic stays on the same local clock through DST transitions', () => {
  const script = `
    const c = require(${JSON.stringify(path.resolve(__dirname, 'config/assets/media-calendar.js'))});
    const before = new Date(2026, 2, 7, 12);
    const after = c.addDays(before, 1);
    if (c.dateKey(after) !== '2026-03-08' || after.getHours() !== 12) process.exit(2);
    const next = c.addDays(after, 1);
    if (c.dateKey(next) !== '2026-03-09' || next.getHours() !== 12) process.exit(3);
    const fall = c.addDays(new Date(2026, 9, 31, 12), 1);
    if (c.dateKey(fall) !== '2026-11-01' || fall.getHours() !== 12) process.exit(4);
  `;
  const result = spawnSync(process.execPath, ['-e', script], { env: { ...process.env, TZ: 'America/New_York' } });
  assert.equal(result.status, 0, result.stderr.toString());
});

test('ranges use Monday-first weeks and correct leap-year and month boundaries', () => {
  const week = calendar.rangeFor(date('2024-02-29'), 'week');
  assert.equal(calendar.dateKey(week.start), '2024-02-26');
  assert.equal(calendar.dateKey(week.end), '2024-03-04');
  const february = calendar.rangeFor(date('2024-02-29'), 'month');
  assert.equal(calendar.dateKey(february.start), '2024-02-01');
  assert.equal(calendar.dateKey(february.end), '2024-03-01');
  const year = calendar.rangeFor(date('2024-07-08'), 'year');
  assert.equal(calendar.dateKey(year.start), '2024-01-01');
  assert.equal(calendar.dateKey(year.end), '2025-01-01');
  assert.equal(calendar.dateKey(calendar.move(date('2024-01-31'), 'month', 1)), '2024-02-01');
  assert.equal(calendar.dateKey(calendar.move(date('2024-12-30'), 'week', 1)), '2025-01-06');
});

test('visible events use browser-local days and an exclusive range end', () => {
  const events = [
    { id: 1, kind: 'movie', date: '2026-10-19' },
    { id: 2, kind: 'episode', date: '2026-10-20T00:30:00+02:00' },
    { id: 3, kind: 'movie', date: '2026-10-21' },
  ];
  const visible = calendar.visibleEvents(events, date('2026-10-19'), date('2026-10-21'));
  assert.deepEqual(visible.map(calendar.eventDay), ['2026-10-19', calendar.eventDay(events[1])]);
  assert.equal(visible.some(event => event.id === 3), false);
});

test('same-day episodes group by series identity, with only groups over three expandable', () => {
  const grouped = calendar.groupEvents([
    ...[1, 2, 3, 4].map(number => episode('a', number)),
    ...[1, 2, 3].map(number => episode('b', number)),
  ]);
  assert.equal(grouped.length, 2);
  assert.deepEqual(grouped.map(group => group.events.length), [4, 3]);
  assert.notEqual(grouped[0].key, grouped[1].key);
});

test('movie timestamps retain their release date while episode timestamps follow local time', () => {
  const script = `
    const c = require(${JSON.stringify(path.resolve(__dirname, 'config/assets/media-calendar.js'))});
    const stamp = '2026-10-20T00:00:00Z';
    if (c.eventDay({kind:'movie',date:stamp}) !== '2026-10-20') process.exit(2);
    if (c.eventDay({kind:'episode',date:stamp}) !== '2026-10-19') process.exit(3);
  `;
  const result = spawnSync(process.execPath, ['-e', script], { env: { ...process.env, TZ: 'America/Los_Angeles' } });
  assert.equal(result.status, 0, result.stderr.toString());
});

class FakeNode {
  constructor(tag, ownerDocument) {
    this.tagName = tag;
    this.ownerDocument = ownerDocument;
    this.children = [];
    this.dataset = {};
    this.attributes = {};
    this.listeners = {};
    this.className = '';
    this.textContent = '';
    this.hidden = false;
    this.disabled = false;
    this.open = false;
    this.parentElement = null;
  }
  append(...nodes) {
    for (const node of nodes) {
      node.parentElement = this;
      this.children.push(node);
    }
  }
  replaceChildren(...nodes) {
    this.children.forEach(node => { node.parentElement = null; });
    this.children = [];
    this.append(...nodes);
  }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  click() { this.listeners.click?.({ target: this }); }
  focus() { this.ownerDocument.activeElement = this; }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  closest(selector) {
    for (let node = this; node; node = node.parentElement) {
      if (selector === 'details[data-group]' && node.tagName === 'details' && node.dataset.group) return node;
      if (selector === 'details' && node.tagName === 'details') return node;
    }
    return null;
  }
  querySelectorAll(selector) {
    const match = selector === 'details' ? node => node.tagName === 'details'
      : selector === 'a' ? node => node.tagName === 'a' : () => false;
    return this.children.flatMap(child => [...(match(child) ? [child] : []), ...child.querySelectorAll(selector)]);
  }
}

function mountedFixture() {
  const doc = {
    hidden: false,
    activeElement: null,
    listeners: {},
    createElement(tag) { return new FakeNode(tag, doc); },
    addEventListener(name, callback) { this.listeners[name] = callback; },
  };
  const root = new FakeNode('main', doc);
  root.ownerDocument = doc;
  root.dataset = { seriesBase: '/series/', movieBase: '/movie/' };
  root.isConnected = true;
  root.setAttribute = FakeNode.prototype.setAttribute;
  root.replaceChildren = FakeNode.prototype.replaceChildren;
  const originalFetch = global.fetch;
  const originalSetTimeout = global.setTimeout;
  const originalClearTimeout = global.clearTimeout;
  const requests = [];
  global.setTimeout = () => 1;
  global.clearTimeout = () => {};
  global.fetch = (url, options) => new Promise((resolve, reject) => {
    const request = { url, options, resolve, reject };
    requests.push(request);
  });
  calendar.mount(root, '/gateway');
  const tick = () => new Promise(resolve => setImmediate(resolve));
  return {
    root, doc, requests, tick,
    restore() { global.fetch = originalFetch; global.setTimeout = originalSetTimeout; global.clearTimeout = originalClearTimeout; },
    resolve(request, events, start, end) {
      request.resolve({ ok: true, json: async () => ({ start, end, events }) });
    },
    reject(request, error = new Error('fixture failure')) { request.reject(error); },
    buttons() { return root.children[0].children.flatMap(node => node.children); },
    all(selector) { return root.querySelectorAll(selector); },
    text(node) { return [node.textContent, ...node.children.map(child => this.text(child))].filter(Boolean).join(' '); },
  };
}

test('mount defaults large series groups closed, leaves small groups as links, and retains expansion on refresh', async () => {
  const page = mountedFixture();
  try {
    const first = page.requests[0];
    const start = new URL(first.url, 'http://local').searchParams.get('start');
    const end = new URL(first.url, 'http://local').searchParams.get('end');
    const releases = [...[1, 2, 3, 4].map(number => episode('a', number, 'Same title', start)), ...[1, 2, 3].map(number => episode('b', number, 'Same title', start))];
    page.resolve(first, releases, start, end);
    await page.tick();
    const details = page.all('details');
    assert.equal(details.length, 1);
    assert.equal(details[0].open, false);
    assert.match(page.text(details[0].children[0]), /4 episodes/);
    const links = page.all('a');
    assert.equal(links.length, 7);
    assert.ok(links.every(link => link.href.startsWith('/series/')));
    assert.equal(details[0].querySelectorAll('a').length, 4);
    assert.equal(page.root.children[4].querySelectorAll('a').length, 7);
    details[0].open = true;
    details[0].listeners.toggle?.();
    page.doc.listeners.visibilitychange();
    const refresh = page.requests.at(-1);
    assert.notEqual(refresh, first);
    page.resolve(refresh, [...releases, episode('a', 5, 'Same title', start)], start, end);
    await page.tick();
    assert.equal(page.all('details')[0].open, true);
    assert.equal(page.all('details')[0].querySelectorAll('a').length, 5);
  } finally { page.restore(); }
});

test('calendar controls request adjacent ranges and switch between day, week, month, and year', async () => {
  const page = mountedFixture();
  try {
    const initial = page.requests[0];
    const initialURL = new URL(initial.url, 'http://local');
    const initialStart = initialURL.searchParams.get('start');
    const initialEnd = initialURL.searchParams.get('end');
    assert.equal(dayOrdinal(initialEnd) - dayOrdinal(initialStart), 7);
    const toolbar = page.root.children[0];
    const navigation = toolbar.children[0];
    navigation.children[2].click();
    const next = page.requests.at(-1);
    const nextURL = new URL(next.url, 'http://local');
    assert.equal(nextURL.searchParams.get('start'), calendar.dateKey(calendar.addDays(date(initialStart), 7)));
    assert.equal(nextURL.searchParams.get('end'), calendar.dateKey(calendar.addDays(date(initialEnd), 7)));
    for (const [index, expectedDays] of [[0, 1], [2, 31], [3, 365]]) {
      toolbar.children[1].children[index].click();
      const request = page.requests.at(-1);
      const url = new URL(request.url, 'http://local');
      const days = dayOrdinal(url.searchParams.get('end')) - dayOrdinal(url.searchParams.get('start'));
      if (expectedDays === 31) assert.ok(days >= 28 && days <= 31);
      else if (expectedDays === 365) assert.ok(days === 365 || days === 366);
      else assert.equal(days, expectedDays);
    }
  } finally { page.restore(); }
});

test('failed refresh keeps the last successful calendar and retry recovers with expansion preserved', async () => {
  const page = mountedFixture();
  try {
    const initial = page.requests[0];
    const url = new URL(initial.url, 'http://local');
    const start = url.searchParams.get('start');
    const end = url.searchParams.get('end');
    const releases = [1, 2, 3, 4].map(number => episode('a', number, 'Last good release', start));
    page.resolve(initial, releases, start, end);
    await page.tick();
    page.all('details')[0].open = true;
    page.all('details')[0].listeners.toggle?.();

    page.doc.listeners.visibilitychange();
    const failedRefresh = page.requests.at(-1);
    page.reject(failedRefresh);
    await page.tick();
    assert.match(page.text(page.root.children[2]), /refresh failed/i);
    assert.equal(page.root.children[3].hidden, false);
    assert.match(page.text(page.root.children[4]), /Last good release/);
    assert.equal(page.all('details')[0].open, true);

    page.root.children[3].click();
    const retry = page.requests.at(-1);
    const updatedEpisode = episode('a', 5, 'Last good release', start);
    updatedEpisode.subtitle = 'Updated release';
    page.resolve(retry, [...releases, updatedEpisode], start, end);
    await page.tick();
    assert.match(page.text(page.root.children[4]), /Updated release/);
    assert.equal(page.all('details')[0].open, true);
    assert.equal(page.all('details')[0].querySelectorAll('a').length, 5);
  } finally { page.restore(); }
});

test('a slower response for the previous range cannot replace the current calendar', async () => {
  const page = mountedFixture();
  try {
    const stale = page.requests[0];
    const staleURL = new URL(stale.url, 'http://local');
    const staleStart = staleURL.searchParams.get('start');
    const staleEnd = staleURL.searchParams.get('end');
    page.root.children[0].children[0].children[2].click();
    const current = page.requests[1];
    const currentURL = new URL(current.url, 'http://local');
    const currentStart = currentURL.searchParams.get('start');
    const currentEnd = currentURL.searchParams.get('end');
    page.resolve(current, [{ kind: 'episode', title: 'Current range', slug: 'current', seriesId: 'current', season: 1, episode: 1, date: currentStart }], currentStart, currentEnd);
    await page.tick();
    page.resolve(stale, [{ kind: 'episode', title: 'Stale range', slug: 'stale', seriesId: 'stale', season: 1, episode: 1, date: staleStart }], staleStart, staleEnd);
    await page.tick();
    assert.match(page.text(page.root.children[4]), /Current range/);
    assert.doesNotMatch(page.text(page.root.children[4]), /Stale range/);
  } finally { page.restore(); }
});

