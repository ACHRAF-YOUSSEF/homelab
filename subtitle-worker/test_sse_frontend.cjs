const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const updatesCode = app.slice(app.indexOf('const jobsUpdates = {'), app.indexOf("$('refresh-library').onclick"));
const lifecycleCode = app.slice(app.indexOf("window.addEventListener('pagehide'"));

function fixture({ supported = true, constructorFails = false } = {}) {
  const timers = new Map();
  const connections = [];
  const requests = [];
  const lifecycle = new Map();
  const indicator = { dataset: {}, textContent: '', className: '' };
  const list = { errors: [], replaceChildren(item) { this.errors.push(item.text); } };
  const state = { jobs: [], jobFilter: 'running', openReviews: new Set(['job:en']), openSteps: new Set(['job']) };
  let now = 0;
  let timerId = 0;
  let renders = 0;

  class MockEventSource {
    constructor(url) {
      if (constructorFails) throw Error('EventSource could not start');
      this.url = url;
      this.listeners = new Map();
      this.closed = false;
      connections.push(this);
    }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    send(jobs) { this.listeners.get('jobs')({ data: JSON.stringify(jobs), lastEventId: 'test:1' }); }
    fail() { this.onerror(); }
    close() { this.closed = true; }
  }

  const schedule = (callback, delay, interval) => {
    const id = ++timerId;
    timers.set(id, { callback, delay, interval, due: now + delay });
    return id;
  };
  const context = vm.createContext({
    state,
    $: id => id === 'jobs-connection' ? indicator : list,
    ui: { empty: 'empty' },
    element: (tag, className, text) => ({ tag, className, text }),
    renderJobs: () => { renders += 1; },
    api: url => new Promise((resolve, reject) => requests.push({ url, resolve, reject })),
    setTimeout: (callback, delay) => schedule(callback, delay, false),
    clearTimeout: id => timers.delete(id),
    setInterval: (callback, delay) => schedule(callback, delay, true),
    clearInterval: id => timers.delete(id),
    window: { addEventListener: (name, callback) => lifecycle.set(name, callback) },
    ...(supported ? { EventSource: MockEventSource } : {}),
  });
  vm.runInContext(updatesCode + '\n' + lifecycleCode, context);
  return {
    state, indicator, list, connections, requests, timers,
    run: code => vm.runInContext(code, context),
    event: (name, event = {}) => lifecycle.get(name)(event),
    renders: () => renders,
    advance(ms) {
      const target = now + ms;
      while (true) {
        const next = [...timers.entries()].filter(([, timer]) => timer.due <= target)
          .sort((a, b) => a[1].due - b[1].due)[0];
        if (!next) break;
        const [id, timer] = next;
        now = timer.due;
        if (timer.interval) timer.due += timer.delay;
        else timers.delete(id);
        timer.callback();
      }
      now = target;
    },
  };
}

const flush = () => new Promise(resolve => setImmediate(resolve));
const job = progress => ({ id: 'example', filename: 'Example.mkv', progress });

test('healthy SSE creates one connection and updates jobs without recurring fetches', () => {
  const page = fixture();
  page.run('startJobsUpdates(); startJobsUpdates();');
  assert.equal(page.connections.length, 1);
  assert.equal(page.connections[0].url, '/api/jobs/events');
  assert.equal(page.indicator.dataset.status, 'connecting');
  page.connections[0].send([job(20)]);
  assert.equal(page.state.jobs[0].progress, 20);
  assert.equal(page.indicator.dataset.status, 'live');
  assert.equal(page.renders(), 1);
  page.advance(30000);
  assert.equal(page.requests.length, 0);
  assert.equal(page.timers.size, 0);
  assert.equal(page.state.jobFilter, 'running');
  assert.deepEqual([...page.state.openReviews], ['job:en']);
  assert.deepEqual([...page.state.openSteps], ['job']);
});

test('slow refreshes and errors cannot overwrite newer SSE snapshots', async () => {
  const page = fixture();
  page.run('startJobsUpdates(); void loadJobs();');
  page.connections[0].send([job(50)]);
  page.requests[0].resolve([job(10)]);
  await flush();
  assert.equal(page.state.jobs[0].progress, 50);
  assert.equal(page.renders(), 1);
  page.run('void loadJobs();');
  page.connections[0].send([job(60)]);
  page.requests[1].reject(Error('Disconnected'));
  await flush();
  assert.equal(page.state.jobs[0].progress, 60);
  assert.equal(page.list.errors.length, 0);
});

test('overlapping manual fetches apply only the most recent request', async () => {
  const page = fixture();
  page.run('void loadJobs(); void loadJobs();');
  page.requests[1].resolve([job(70)]);
  await flush();
  page.requests[0].resolve([job(30)]);
  await flush();
  assert.equal(page.state.jobs[0].progress, 70);
  assert.equal(page.renders(), 1);
});

test('sustained outage starts restrained polling and recovery stops it', async () => {
  const page = fixture();
  page.run('startJobsUpdates();');
  const source = page.connections[0];
  source.send([job(10)]);
  source.fail();
  page.advance(9000);
  source.fail();
  assert.equal(page.requests.length, 0);
  page.advance(1000);
  assert.equal(page.indicator.dataset.status, 'fallback');
  assert.equal(page.requests.length, 1);
  page.advance(10000);
  assert.equal(page.requests.length, 1, 'slow fallback requests must not overlap');
  page.requests[0].resolve([job(20)]);
  await flush();
  page.advance(5000);
  assert.equal(page.requests.length, 2);
  source.send([job(80)]);
  assert.equal(page.indicator.dataset.status, 'live');
  assert.equal(page.timers.size, 0);
  page.requests[1].resolve([job(30)]);
  await flush();
  assert.equal(page.state.jobs[0].progress, 80);
  page.advance(30000);
  assert.equal(page.requests.length, 2);
  assert.equal(page.connections.length, 1, 'EventSource performs its own reconnects');
});

test('short disconnect recovers without enabling polling', () => {
  const page = fixture();
  page.run('startJobsUpdates();');
  const source = page.connections[0];
  source.fail();
  assert.equal(page.indicator.dataset.status, 'reconnecting');
  page.advance(3000);
  source.send([job(40)]);
  page.advance(20000);
  assert.equal(page.indicator.dataset.status, 'live');
  assert.equal(page.requests.length, 0);
});

test('unsupported or unavailable EventSource uses polling immediately', () => {
  for (const options of [{ supported: false }, { constructorFails: true }]) {
    const page = fixture(options);
    page.run('startJobsUpdates();');
    assert.equal(page.indicator.dataset.status, 'fallback');
    assert.equal(page.requests.length, 1);
    assert.equal(page.requests[0].url, '/api/jobs');
    assert.equal(page.timers.size, 1);
  }
});

test('pagehide releases connection and timers; bfcache restore creates one new stream', async () => {
  const page = fixture();
  page.run('startJobsUpdates(); void loadJobs();');
  const oldSource = page.connections[0];
  oldSource.send([job(50)]);
  oldSource.fail();
  page.event('pagehide');
  assert.equal(oldSource.closed, true);
  assert.equal(page.timers.size, 0);
  page.event('pageshow', { persisted: false });
  assert.equal(page.connections.length, 1);
  page.event('pageshow', { persisted: true });
  assert.equal(page.connections.length, 2);
  oldSource.send([job(10)]);
  assert.equal(page.state.jobs[0].progress, 50);
  page.requests[0].resolve([job(20)]);
  await flush();
  assert.equal(page.state.jobs[0].progress, 50);
  page.connections[1].send([job(90)]);
  assert.equal(page.state.jobs[0].progress, 90);
  assert.equal(page.indicator.dataset.status, 'live');
});

test('invalid stream events preserve current jobs and fall back if they persist', () => {
  const page = fixture();
  page.run('startJobsUpdates();');
  const source = page.connections[0];
  source.send([job(50)]);
  source.listeners.get('jobs')({ data: 'not json' });
  assert.equal(page.state.jobs[0].progress, 50);
  page.advance(10000);
  assert.equal(page.indicator.dataset.status, 'fallback');
  source.send([job(75)]);
  assert.equal(page.state.jobs[0].progress, 75);
  assert.equal(page.indicator.dataset.status, 'live');
});
