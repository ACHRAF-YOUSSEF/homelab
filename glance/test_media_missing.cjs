const assert = require('node:assert/strict');
const test = require('node:test');
const missing = require('./config/assets/media-missing.js');

const episode = (id, seriesId = 1, season = 1, number = id, title = 'Example show') => ({
  kind: 'episode', id, seriesId, season, episode: number, title, subtitle: `Episode ${number}`, slug: `show-${seriesId}`, year: null,
});
const movie = (id, title = 'Example movie') => ({ kind: 'movie', id, title, slug: `movie-${id}`, year: 2026 });

test('groups shows by identity, seasons numerically, specials first, and episodes in order', () => {
  const result = missing.groupItems([
    episode(5, 1, 10, 2), episode(4, 1, 2, 3), episode(3, 1, 2, 1), episode(2, 1, 0, 1),
    episode(6, 2, 1, 1), movie(1, 'Zebra'), movie(2, 'Alpha'),
  ]);
  assert.equal(result.shows.length, 2);
  assert.deepEqual(result.shows[0].seasons.map(season => season.number), [0, 2, 10]);
  assert.deepEqual(result.shows[0].seasons[1].episodes.map(item => item.episode), [1, 3]);
  assert.deepEqual(result.movies.map(item => item.title), ['Alpha', 'Zebra']);
});

class FakeNode {
  constructor(tag, doc) {
    Object.assign(this, { tagName: tag, ownerDocument: doc, children: [], dataset: {}, attributes: {}, listeners: {}, textContent: '', open: false });
  }
  append(...children) { children.forEach(child => { child.parentElement = this; this.children.push(child); }); }
  replaceChildren(...children) { this.children.forEach(child => { child.parentElement = null; }); this.children = []; this.append(...children); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  click() { this.listeners.click?.(); }
  focus() { this.ownerDocument.activeElement = this; }
  contains(node) { return this === node || this.children.some(child => child.contains(node)); }
  querySelectorAll(selector) {
    const matches = node => selector === '[data-focus]' ? node.dataset.focus !== undefined
      : selector === '.media-missing-list' ? node.className === 'media-missing-list' : node.tagName === selector;
    return this.children.flatMap(child => [...(matches(child) ? [child] : []), ...child.querySelectorAll(selector)]);
  }
}

function fixture() {
  const doc = { hidden: false, activeElement: null, listeners: {}, createElement(tag) { return new FakeNode(tag, doc); }, addEventListener(name, callback) { this.listeners[name] = callback; } };
  const root = new FakeNode('main', doc);
  root.isConnected = true;
  root.dataset = { seriesBase: '/series/', movieBase: '/movie/' };
  const originals = { fetch: global.fetch, setTimeout: global.setTimeout, clearTimeout: global.clearTimeout };
  const requests = [], timers = [];
  global.fetch = (url, options) => new Promise((resolve, reject) => requests.push({ url, options, resolve, reject }));
  global.setTimeout = (callback, ms) => { timers.push({ callback, ms }); return timers.length; };
  global.clearTimeout = () => {};
  missing.mount(root, '/gateway');
  return {
    root, doc, requests, timers,
    tick: () => new Promise(resolve => setImmediate(resolve)),
    resolve(items, providers = { sonarr: true, radarr: true }, request = requests.at(-1)) { request.resolve({ ok: true, json: async () => ({ items, providers }) }); },
    text(node = root) { return [node.textContent, ...node.children.map(child => this.text(child))].filter(Boolean).join(' '); },
    refresh() { doc.listeners.visibilitychange(); },
    restore() { Object.assign(global, originals); },
  };
}

test('multiple episodes collapse by show and season, single episodes and movies link directly', async () => {
  const page = fixture();
  try {
    assert.equal(page.requests[0].url, '/gateway/live/missing');
    assert.equal(page.requests[0].options.cache, 'no-store');
    page.resolve([episode(1), episode(2, 1, 2), episode(3, 1, 0), episode(4, 2), movie(1)]);
    await page.tick();
    const details = page.root.querySelectorAll('details');
    assert.equal(details.length, 4);
    assert.ok(details.every(detail => !detail.open));
    assert.match(page.text(details[0].children[0]), /3 episodes/);
    assert.match(page.text(details[1].children[0]), /Specials/);
    assert.match(page.text(details[2].children[0]), /Season 1/);
    assert.match(page.text(details[3].children[0]), /Season 2/);
    assert.equal(page.root.querySelectorAll('a').length, 5);
    assert.equal(details[0].querySelectorAll('a').length, 3);
    assert.match(page.text(page.root.children[0]), /4 episodes in 2 shows · 1 movie/);
    assert.equal(page.root.attributes['aria-busy'], 'false');
    assert.ok(page.timers.some(timer => timer.ms === 30000));
  } finally { page.restore(); }
});

test('refresh retains open shows, seasons, and focused episode; unchanged refresh leaves DOM intact', async () => {
  const page = fixture();
  try {
    const items = [episode(1), episode(2)];
    page.resolve(items);
    await page.tick();
    const details = page.root.querySelectorAll('details');
    details.forEach(detail => { detail.open = true; });
    page.root.querySelectorAll('a')[1].focus();
    const content = page.root.children[2];
    content.querySelectorAll('.media-missing-list')[0].scrollTop = 120;
    page.refresh();
    page.resolve(items);
    await page.tick();
    assert.equal(content.querySelectorAll('details')[0], details[0]);
    page.refresh();
    page.resolve([...items, episode(3)]);
    await page.tick();
    assert.ok(content.querySelectorAll('details').every(detail => detail.open));
    assert.equal(page.doc.activeElement.dataset.focus, 'episode:2');
    assert.ok(content.contains(page.doc.activeElement));
    assert.equal(content.querySelectorAll('.media-missing-list')[0].scrollTop, 120);
  } finally { page.restore(); }
});

test('failure shows retry and preserves last successful inventory, then recovers', async () => {
  const page = fixture();
  try {
    page.requests[0].resolve({ ok: false });
    await page.tick();
    assert.match(page.text(page.root.children[0]), /Could not load/);
    assert.equal(page.root.children[1].hidden, false);
    page.root.children[1].click();
    page.resolve([movie(1)]);
    await page.tick();
    page.refresh();
    page.requests.at(-1).reject(new Error('fixture failure'));
    await page.tick();
    assert.match(page.text(page.root.children[0]), /last successful update/);
    assert.match(page.text(page.root.children[2]), /Example movie/);
    page.root.children[1].click();
    page.resolve([]);
    await page.tick();
    assert.match(page.text(), /No missing aired episodes/);
    assert.match(page.text(), /No missing movies/);
    assert.equal(page.root.children[1].hidden, true);
  } finally { page.restore(); }
});

test('disabled provider is distinct from empty inventory; titles stay text and slugs stay path segments', async () => {
  const page = fixture();
  try {
    const item = movie(1, '<img src=x onerror=alert(1)>');
    item.slug = '../unsafe?x=y';
    page.resolve([item], { sonarr: false, radarr: true });
    await page.tick();
    assert.match(page.text(), /Sonarr API key is not configured/);
    assert.doesNotMatch(page.text(), /No missing aired episodes/);
    const link = page.root.querySelectorAll('a')[0];
    assert.equal(link.children[0].textContent, item.title);
    assert.equal(link.href, '/movie/..%2Funsafe%3Fx%3Dy');
    assert.equal(page.root.querySelectorAll('img').length, 0);
  } finally { page.restore(); }
});

test('hidden page cancels work and ignores late responses, then refreshes when visible', async () => {
  const page = fixture();
  try {
    const stale = page.requests[0];
    page.doc.hidden = true;
    page.refresh();
    assert.equal(stale.options.signal.aborted, true);
    assert.equal(page.requests.length, 1);
    assert.equal(page.root.attributes['aria-busy'], 'false');
    page.doc.hidden = false;
    page.refresh();
    page.resolve([movie(2, 'Current')]);
    await page.tick();
    page.resolve([movie(1, 'Stale')], { sonarr: true, radarr: true }, stale);
    await page.tick();
    assert.match(page.text(), /Current/);
    assert.doesNotMatch(page.text(), /Stale/);
  } finally { page.restore(); }
});
