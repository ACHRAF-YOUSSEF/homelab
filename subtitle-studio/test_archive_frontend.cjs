const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const code = app.slice(app.indexOf('function stepActionPending('), app.indexOf('function renderSteps('));

function fixture({ fail = false } = {}) {
  const nodes = new Map([
    ['archive-feedback', { hidden: true, textContent: '', setAttribute(name, value) { this[name] = value; } }],
    ['archive-inactive-jobs', { disabled: false }],
  ]);
  const requests = [];
  const state = { pendingStepActions: new Set(), pendingReviews: new Set(), reviewingMissing: false, jobActionFeedback: new Map() };
  let loads = 0;
  const context = vm.createContext({
    state, $: id => nodes.get(id),
    renderJobs() {},
    element: (tag, className, textContent) => ({ tag, className, textContent, children: [], append(...items) { this.children.push(...items); }, setAttribute(name, value) { this[name] = value; } }),
    api: async (url, options) => { requests.push({ url, options }); if (fail) throw Error('Archive reached its size limit'); return { archived: 3 }; },
    loadJobs: async () => { loads += 1; },
  });
  vm.runInContext(code, context);
  return { nodes, requests, state, context, loads: () => loads };
}

test('bulk archive is a same-origin JSON mutation and reports preserved files', async () => {
  const page = fixture();
  await page.context.archiveInactiveJobs();
  assert.equal(page.requests[0].url, '/api/jobs/archive-inactive');
  assert.equal(page.requests[0].options.method, 'POST');
  assert.equal(page.requests[0].options.headers['Content-Type'], 'application/json');
  assert.match(page.nodes.get('archive-feedback').textContent, /3 jobs archived.*files and caches are kept/);
  assert.equal(page.loads(), 1);
  assert.equal(page.nodes.get('archive-inactive-jobs').disabled, false);
});

test('archive capacity errors remain visible and permit retry', async () => {
  const page = fixture({ fail: true });
  await page.context.archiveInactiveJobs();
  assert.equal(page.nodes.get('archive-feedback').role, 'alert');
  assert.match(page.nodes.get('archive-feedback').textContent, /size limit/);
  assert.equal(page.nodes.get('archive-inactive-jobs').disabled, false);
  assert.equal(page.loads(), 0);
});

test('archive and restore buttons choose their own routes and report status outside the filtered list', async () => {
  for (const action of ['archive', 'restore']) {
    const page = fixture();
    await page.context.controlJob({ id: 'abcdef123456' }, null, action);
    assert.equal(page.requests[0].url, `/api/jobs/abcdef123456/${action}`);
    assert.match(page.nodes.get('archive-feedback').textContent, /Job archived|Job restored/);
    assert.equal(page.state.pendingStepActions.size, 0);
  }
});

test('inactive cards offer archive or restore with a record download; busy cards offer cancellation', () => {
  const page = fixture();
  for (const [options, expected] of [[{ can_archive: true }, 'Archive job'], [{ can_restore: true, archived_at: 'now' }, 'Restore job'], [{ status: 'running' }, 'Cancel job']]) {
    const card = { children: [], append(...items) { this.children.push(...items); } };
    page.context.renderJobControls(card, { id: 'abcdef123456', status: 'completed', ...options });
    const actions = card.children[0];
    assert.equal(actions.children[0].textContent, expected);
    if (expected !== 'Cancel job') assert.equal(actions.children[1].href, '/api/jobs/abcdef123456/record');
  }
});
