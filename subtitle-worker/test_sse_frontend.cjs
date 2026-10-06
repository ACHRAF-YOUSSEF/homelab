const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const updatesCode = app.slice(app.indexOf('const jobsUpdates = {'), app.indexOf("$('refresh-library').onclick"));
const lifecycleCode = app.slice(app.indexOf("window.addEventListener('pagehide'"));
const paginationHandlers = app.slice(app.lastIndexOf("for (const button of $('job-filters').querySelectorAll('[data-job-filter]'))"), app.indexOf('initializeWorkspaceTabs();'));

function snapshot(jobs, { page = 1, page_size = 5, total = (page - 1) * page_size + jobs.length, total_all = total, page_count = Math.max(1, Math.ceil(total / page_size)) } = {}) {
  return { jobs, page, page_size, total, total_all, page_count };
}

function fixture({ supported = true, constructorFails = false } = {}) {
  const timers = new Map();
  const connections = [];
  const requests = [];
  const lifecycle = new Map();
  const indicator = { dataset: {}, textContent: '', className: '' };
  const list = { errors: [], replaceChildren(item) { this.errors.push(item.text); } };
  const filters = ['all', 'completed', 'failed', 'running', 'queued', 'cancelled'].map(jobFilter => ({ dataset: { jobFilter } }));
  const nodes = new Map([
    ['jobs-connection', indicator], ['jobs-list', list], ['jobs-content', { scrollTop: 120 }],
    ['jobs-prev', { disabled: false }], ['jobs-next', { disabled: false }],
    ['jobs-page-size', { value: '5' }], ['jobs-page-summary', { textContent: '' }],
    ['jobs-pagination', { attributes: {}, setAttribute(name, value) { this.attributes[name] = value; } }],
    ['job-filters', { querySelectorAll: () => filters }],
  ]);
  const state = { jobs: [], jobFilter: 'running', jobPage: 1, jobPageSize: 5, jobTotal: 0, jobTotalAll: 0, jobPageCount: 1, jobsLoading: true, openReviews: new Set(['job:en']), openSteps: new Set(['job']) };
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
    send(jobs) {
      const query = new URL(this.url, 'http://localhost').searchParams;
      const data = Array.isArray(jobs) ? snapshot(jobs, { page: Number(query.get('page')), page_size: Number(query.get('page_size')) }) : jobs;
      this.listeners.get('jobs')({ data: JSON.stringify(data), lastEventId: 'test:1' });
    }
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
    $: id => nodes.get(id),
    ui: { empty: 'empty' },
    element: (tag, className, text) => ({ tag, className, text }),
    renderJobs: () => { renders += 1; context.renderJobsPagination(); },
    api: url => new Promise((resolve, reject) => {
      const query = new URL(url, 'http://localhost').searchParams;
      requests.push({ url, resolve: data => resolve(Array.isArray(data) ? snapshot(data, { page: Number(query.get('page')), page_size: Number(query.get('page_size')) }) : data), reject });
    }),
    URLSearchParams,
    setTimeout: (callback, delay) => schedule(callback, delay, false),
    clearTimeout: id => timers.delete(id),
    setInterval: (callback, delay) => schedule(callback, delay, true),
    clearInterval: id => timers.delete(id),
    window: { addEventListener: (name, callback) => lifecycle.set(name, callback) },
    ...(supported ? { EventSource: MockEventSource } : {}),
  });
  vm.runInContext(updatesCode + '\n' + paginationHandlers + '\n' + lifecycleCode, context);
  return {
    state, indicator, list, connections, requests, timers, nodes, filters,
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
  assert.equal(page.connections[0].url, '/api/jobs/events?page=1&page_size=5&status=running');
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
    assert.equal(page.requests[0].url, '/api/jobs?page=1&page_size=5&status=running');
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

test('paged snapshot exposes server totals and boundary controls', () => {
  const page = fixture();
  page.run('startJobsUpdates();');
  page.connections[0].send(snapshot([job(50)], { total: 12, total_all: 45 }));
  assert.equal(page.nodes.get('jobs-prev').disabled, true);
  assert.equal(page.nodes.get('jobs-next').disabled, false);
  assert.equal(page.nodes.get('jobs-page-size').value, '5');
  assert.equal(page.nodes.get('jobs-page-summary').textContent, 'Page 1 of 3 · 1–5 of 12 jobs · 45 total');
  assert.equal(page.state.jobFilter, 'running');
  assert.equal(page.state.jobTotal, 12);
  assert.equal(page.state.jobTotalAll, 45);
  assert.equal(page.nodes.get('jobs-content').scrollTop, 120, 'live snapshots preserve scrolling');
});

test('page change resubscribes and excludes old events and fetches', async () => {
  const page = fixture();
  page.run('startJobsUpdates(); void loadJobs();');
  const oldSource = page.connections[0];
  oldSource.send(snapshot([job(10)], { total: 15 }));
  page.nodes.get('jobs-next').onclick();
  assert.equal(oldSource.closed, true);
  assert.equal(page.state.jobPage, 2);
  assert.equal(page.connections[1].url, '/api/jobs/events?page=2&page_size=5&status=running');
  assert.equal(page.requests[1].url, '/api/jobs?page=2&page_size=5&status=running');
  assert.equal(page.nodes.get('jobs-content').scrollTop, 0);
  oldSource.send(snapshot([job(99)], { total: 15 }));
  page.requests[0].resolve(snapshot([job(98)], { total: 15 }));
  await flush();
  assert.equal(page.state.jobs.length, 0);
  page.connections[1].send(snapshot([job(20)], { page: 2, total: 15 }));
  page.requests[1].resolve(snapshot([job(19)], { page: 2, total: 15 }));
  await flush();
  assert.equal(page.state.jobs[0].progress, 20);
  assert.equal(page.state.jobPage, 2);
  assert.deepEqual([...page.state.openReviews], ['job:en']);
});

test('filter and page size actions reset to page one and use matching stream and fallback queries', () => {
  const page = fixture();
  page.run('state.jobPage = 3; startJobsUpdates();');
  page.connections[0].send(snapshot([job(10)], { page: 3, total: 15 }));
  page.filters.find(button => button.dataset.jobFilter === 'failed').onclick();
  assert.equal(page.state.jobPage, 1);
  assert.equal(page.state.jobFilter, 'failed');
  assert.equal(page.connections[1].url, '/api/jobs/events?page=1&page_size=5&status=failed');
  page.connections[1].send(snapshot([job(20)], { total: 13, total_all: 35 }));
  page.nodes.get('jobs-page-size').value = '10';
  page.nodes.get('jobs-page-size').onchange();
  assert.equal(page.state.jobPageSize, 10);
  assert.equal(page.state.jobPage, 1);
  assert.equal(page.connections[2].url, '/api/jobs/events?page=1&page_size=10&status=failed');
  page.connections[2].fail();
  page.advance(10000);
  assert.equal(page.requests.at(-1).url, '/api/jobs?page=1&page_size=10&status=failed');
});

test('server clamp synchronizes the subscribed page without resetting scroll', () => {
  const page = fixture();
  page.run('state.jobPage = 3; startJobsUpdates();');
  const oldSource = page.connections[0];
  oldSource.send(snapshot([job(20)], { page: 2, total: 7 }));
  assert.equal(page.state.jobPage, 2);
  assert.equal(oldSource.closed, true);
  assert.equal(page.connections[1].url, '/api/jobs/events?page=2&page_size=5&status=running');
  assert.equal(page.nodes.get('jobs-content').scrollTop, 120);
  assert.equal(page.nodes.get('jobs-next').disabled, true);
  page.connections[1].send(snapshot([job(30)], { page: 2, total: 15 }));
  assert.equal(page.state.jobPage, 2, 'new inserts must not jump back to the formerly requested page');
  assert.equal(page.connections.length, 2);
});

test('empty paged history has a valid first page and disabled navigation', () => {
  const page = fixture();
  page.run('startJobsUpdates();');
  page.connections[0].send(snapshot([]));
  assert.equal(page.state.jobPageCount, 1);
  assert.equal(page.nodes.get('jobs-prev').disabled, true);
  assert.equal(page.nodes.get('jobs-next').disabled, true);
  assert.equal(page.nodes.get('jobs-page-summary').textContent, 'Page 1 of 1 · 0–0 of 0 jobs · 0 total');
  assert.equal(page.indicator.dataset.status, 'live');
});
