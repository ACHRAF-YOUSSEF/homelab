const test = require('node:test');
const assert = require('node:assert/strict');
const { create } = require('../web/live.js');

test('default browser environment binds native timers to Window', () => {
  const { readFileSync } = require('node:fs');
  const { runInNewContext } = require('node:vm');
  const window = { addEventListener() {} }, document = { hidden: false, addEventListener() {} };
  const calls = [];
  for (const name of ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval']) {
    window[name] = function () { assert.equal(this, window, `${name} requires Window receiver`); calls.push(name); return 1; };
  }
  let source;
  class EventSource {
    constructor() { source = this; }
    addEventListener() {}
    close() {}
  }
  runInNewContext(readFileSync(require.resolve('../web/live.js'), 'utf8'), {
    window, document, EventSource, performance: { now: () => 0 }, ...Object.fromEntries(
      ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval'].map(name => [name, window[name]])),
  });
  const client = window.SubforgeLive.create({});
  client.start();
  source.readyState = 2; source.onerror();
  client.stop();
  for (const name of ['setTimeout', 'clearTimeout', 'setInterval', 'clearInterval']) assert.ok(calls.includes(name));
});

function fixture() {
  const streams = [], states = [], status = [], system = [], workers = [], timeouts = new Map(), intervals = new Map();
  let now = 1000, next = 0;
  const target = () => ({ hidden: false, listeners: {}, addEventListener(name, callback) { this.listeners[name] = callback; } });
  const document = target(), window = target();
  class Source {
    constructor(url) { this.url = url; this.listeners = {}; this.readyState = 0; this.closed = 0; streams.push(this); }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    close() { this.closed++; this.readyState = 2; }
    send(name, value) { this.listeners[name]?.({ data: typeof value === 'string' ? value : JSON.stringify(value) }); }
  }
  const env = { document, window, EventSource: Source, now: () => now,
    setTimeout(callback, delay) { const id = ++next; timeouts.set(id, { callback, delay }); return id; },
    clearTimeout(id) { timeouts.delete(id); },
    setInterval(callback) { const id = ++next; intervals.set(id, callback); return id; },
    clearInterval(id) { intervals.delete(id); } };
  const client = create({ status: value => status.push(value), system: value => system.push(value), workers: value => workers.push(value), connection: value => states.push(value) }, env);
  return { client, streams, states, status, system, workers, document, window, timeouts, intervals, setNow(value) { now = value; } };
}
const snapshot = () => ({ history: [], queue: [], logs: [], roots: [], active_jobs: [{ id: 'a', processed: 1 }] });

test('native named events deliver jobs and measurements without snapshot HTTP polling', () => {
  const page = fixture(); page.client.start();
  assert.equal(page.streams[0].url, '/api/events');
  page.streams[0].send('status', snapshot());
  page.streams[0].send('system', { uptime_seconds: 100, resource_usage: {} });
  page.streams[0].send('workers', { allocation: {}, workers: [{ id: 1 }] });
  assert.equal(page.status[0].active_jobs[0].processed, 1);
  assert.equal(page.system.length, 1); assert.equal(page.workers.length, 1);
  assert.equal(page.states.at(-1), 'connected');
  assert.equal(page.timeouts.size, 0);
});

test('invalid events do not replace prior data, and later valid events recover', () => {
  const page = fixture(); page.client.start(); const source = page.streams[0];
  source.send('status', snapshot());
  for (const data of ['{', '{}', 'null', 'x'.repeat(4 * 1024 * 1024 + 1)]) source.send('status', data);
  for (const malformed of [
    {...snapshot(), active_jobs:'bad', running:null}, {...snapshot(), running:7},
    {...snapshot(), active_jobs:[null]}, {...snapshot(), history:[null]},
    {...snapshot(), tasks:{tasks:'bad'}},
  ]) source.send('status', malformed);
  source.send('system', { uptime_seconds: 5 });
  assert.equal(page.status.length, 1); assert.equal(page.system.length, 0);
  assert.equal(page.states.at(-1), 'invalid');
  source.send('status', snapshot()); assert.equal(page.status.length, 2);
  assert.equal(page.states.at(-1), 'connected');
});

test('hidden and pagehide close streams; visible and pageshow reconnect once', () => {
  const page = fixture(); page.client.start(); page.client.start(); assert.equal(page.streams.length, 1);
  page.document.hidden = true; page.document.listeners.visibilitychange();
  assert.equal(page.streams[0].closed, 1); assert.equal(page.intervals.size, 0);
  page.document.hidden = false; page.document.listeners.visibilitychange();
  assert.equal(page.streams.length, 2);
  page.window.listeners.pagehide(); page.document.listeners.visibilitychange();
  assert.equal(page.streams.length, 2);
  page.window.listeners.pageshow(); page.window.listeners.pageshow();
  assert.equal(page.streams.length, 3);
  page.streams[1].send('status', snapshot()); assert.equal(page.status.length, 0, 'closed streams cannot write late data');
  page.client.stop(); page.document.listeners.visibilitychange(); assert.equal(page.streams.length, 3);
});

test('native reconnects are retained; terminal failures retry with bounded backoff', () => {
  const page = fixture(); page.client.start(); let source = page.streams[0];
  source.readyState = 0; source.onerror(); assert.equal(page.timeouts.size, 0);
  for (let attempt = 0; attempt < 8; attempt++) {
    source.readyState = 2; source.onerror();
    const [id, pending] = [...page.timeouts.entries()][0];
    assert.ok(pending.delay <= 30000);
    page.timeouts.delete(id); pending.callback(); source = page.streams.at(-1);
  }
  assert.equal(page.streams.length, 9);
  source.send('status', snapshot()); source.readyState = 2; source.onerror();
  assert.equal([...page.timeouts.values()][0].delay, 1000);
});

test('status heartbeat watchdog reconnects silent streams without discarding rendered data', () => {
  const page = fixture(); page.client.start(); page.streams[0].send('status', snapshot());
  page.setNow(39000); [...page.intervals.values()][0](); assert.equal(page.streams[0].closed, 0);
  page.setNow(42000); [...page.intervals.values()][0](); assert.equal(page.streams[0].closed, 1);
  assert.equal(page.status.length, 1); assert.equal(page.timeouts.size, 1);
});
