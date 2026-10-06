const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const renderCode = app.slice(app.indexOf('function rememberJobDisclosure('), app.indexOf('const jobsUpdates ='));

function fixture(jobs) {
  const document = { activeElement: null };
  class Node {
    constructor(tag, className = '', text = '') {
      this.tag = tag;
      this.className = className;
      this.textContent = text;
      this.children = [];
      this.attributes = {};
      this.dataset = {};
      this.listeners = new Map();
      this.open = false;
      this.parentElement = null;
      this.classList = { contains: name => this.className.split(' ').includes(name) };
    }
    get isConnected() {
      for (let node = this; node; node = node.parentElement) {
        if (node.connectedRoot) return true;
      }
      return false;
    }
    append(...nodes) {
      for (const node of nodes) {
        if (node.parentElement) node.parentElement.children = node.parentElement.children.filter(item => item !== node);
        node.parentElement = this;
        this.children.push(node);
      }
    }
    replaceChildren(...nodes) {
      for (const node of this.children) node.parentElement = null;
      this.children = [];
      this.append(...nodes);
    }
    querySelectorAll() { return this.children.filter(node => node.tag === 'details' && node.classList.contains('job-card')); }
    setAttribute(name, value) { this.attributes[name] = value; }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    toggle(target = this) { this.listeners.get('toggle')?.({ target }); }
    focus(options) { document.activeElement = this; this.focusOptions = options; }
  }
  const list = new Node('div');
  list.connectedRoot = true;
  const content = { scrollTop: 125 };
  const filters = { querySelectorAll: () => [] };
  const state = { jobs, jobFilter: 'all', jobsLoading: false, openJobs: new Set(), openReviews: new Set(['first:en']), openSteps: new Set(['first']) };
  const helpers = {};
  for (const name of ['renderReviewActivity', 'renderStepActivity', 'renderJobControls', 'renderSteps', 'renderReviewActions', 'renderQuality']) {
    helpers[name] = body => {
      const marker = new Node(name === 'renderSteps' ? 'details' : 'div');
      marker.dataset.section = name;
      body.append(marker);
    };
  }
  const context = vm.createContext({
    state, document, progressHistory: new Map(), visibleJobIds: new Set(),
    $: id => ({ 'jobs-content': content, 'jobs-list': list, 'job-filters': filters })[id],
    ui: { job: 'job-style', stage: 'stage-style', empty: 'empty-style' },
    element: (tag, className, text) => new Node(tag, className, text),
    badgeClasses: status => `badge-${status}`,
    filterClasses: () => '',
    seconds: value => `${value}s`,
    renderJobsPagination: () => {},
    renderProgress: (key, progress, status) => {
      const node = new Node('div', 'progress');
      node.dataset.key = key;
      node.dataset.status = status;
      node.dataset.progress = progress;
      return node;
    },
    ...helpers,
  });
  vm.runInContext(renderCode, context);
  return {
    state, list, content, document,
    render: () => context.renderJobs(),
    cards: () => list.children.filter(node => node.tag === 'details'),
    activity: job => context.jobSummaryActivity(job),
  };
}

function job(id, options = {}) {
  return {
    id, filename: `${id}.mkv`, status: 'completed', stage: 'Complete', progress: 100,
    whisper_model: 'medium', translation_thinking: false, reused: ['transcription'], error: 'Saved generation error',
    outputs: { en: { url: `/api/jobs/${id}/files/en` } },
    ...options,
  };
}

const descendants = node => [node, ...node.children.flatMap(descendants)];
const text = node => descendants(node).map(item => item.textContent).filter(Boolean).join(' ');

test('new job cards use native details and default closed, with compact live summaries', () => {
  const page = fixture([job('first'), job('second')]);
  page.render();
  for (const card of page.cards()) {
    assert.equal(card.open, false);
    assert.equal(card.dataset.jobId, card.children[0].children[0].children[0].textContent.replace('.mkv', ''));
    const [summary, body] = card.children;
    assert.equal(summary.tag, 'summary');
    assert.equal(summary.className, 'job-summary');
    assert.equal(body.className, 'job-body');
    assert.match(text(summary), /Complete · 100%/);
    assert.match(text(summary), /completed/);
    assert.equal(descendants(summary).filter(node => node.tag === 'a').length, 0);
    assert.equal(descendants(summary).find(node => node.className === 'job-chevron').attributes['aria-hidden'], 'true');
    assert.equal(descendants(summary).find(node => node.className === 'progress').dataset.progress, 100);
    assert.match(text(body), /Whisper medium/);
    assert.match(text(body), /Translation thinking off/);
    assert.match(text(body), /Saved generation error/);
    assert.match(text(body), /Reused: transcription/);
    assert.equal(descendants(body).find(node => node.tag === 'a').href, `/api/jobs/${card.dataset.jobId}/files/en`);
    assert.equal(descendants(body).filter(node => node.dataset.section).length, 6);
  }
});

test('each job stores its own disclosure state through SSE rerenders', () => {
  const page = fixture([job('first'), job('second')]);
  page.render();
  page.cards()[0].open = true;
  page.cards()[0].toggle();
  assert.deepEqual([...page.state.openJobs], ['first']);
  page.state.jobs[0].progress = 95;
  page.render();
  assert.equal(page.cards()[0].open, true);
  assert.equal(page.cards()[1].open, false);
  page.cards()[1].open = true;
  page.cards()[1].toggle();
  page.cards()[0].open = false;
  page.cards()[0].toggle();
  assert.deepEqual([...page.state.openJobs], ['second']);
  assert.deepEqual([...page.state.openReviews], ['first:en']);
  assert.deepEqual([...page.state.openSteps], ['first']);
});

test('rerenders capture an opened card before its queued toggle is delivered', () => {
  const page = fixture([job('first')]);
  page.render();
  page.cards()[0].open = true;
  page.render();
  assert.equal(page.cards()[0].open, true);
  assert.deepEqual([...page.state.openJobs], ['first']);
});

test('detached card toggle events cannot undo the current card state', () => {
  const page = fixture([job('first')]);
  page.render();
  const oldCard = page.cards()[0];
  oldCard.open = true;
  oldCard.toggle();
  page.render();
  assert.equal(oldCard.isConnected, false);
  oldCard.open = false;
  oldCard.toggle();
  assert.equal(page.cards()[0].open, true);
  assert.deepEqual([...page.state.openJobs], ['first']);
  page.cards()[0].open = false;
  page.cards()[0].toggle();
  oldCard.open = true;
  oldCard.toggle();
  assert.deepEqual([...page.state.openJobs], []);
});

test('nested disclosure toggles do not alter the outer job state', () => {
  const page = fixture([job('first')]);
  page.render();
  const card = page.cards()[0];
  const nested = descendants(card).find(node => node.dataset.section === 'renderSteps');
  card.open = true;
  card.toggle(nested);
  assert.equal(page.state.openJobs.size, 0);
  card.toggle();
  assert.deepEqual([...page.state.openJobs], ['first']);
});

test('filtering and pagination retain expansion by ID while new jobs remain collapsed', () => {
  const page = fixture([job('first')]);
  page.render();
  page.cards()[0].open = true;
  page.cards()[0].toggle();
  page.state.jobs = [];
  page.render();
  page.state.jobs = [job('new'), job('first')];
  page.render();
  assert.equal(page.cards()[0].open, false);
  assert.equal(page.cards()[1].open, true);
  assert.deepEqual([...page.state.openJobs], ['first']);
});

test('compact summaries display active review or step retry rather than stale generation progress', () => {
  const page = fixture([
    job('review', { review_task: { status: 'running', stage: 'Reviewing French', progress: 42, force: true } }),
    job('retry', { status: 'failed', step_task: { status: 'running', stage: 'Transcribing', progress: 25 } }),
  ]);
  page.render();
  assert.match(text(page.cards()[0].children[0]), /Re-review · Reviewing French · 42%/);
  assert.match(text(page.cards()[1].children[0]), /Step retry · Transcribing · 25%/);
  for (const card of page.cards()) {
    assert.match(text(card.children[0]), /running/);
    assert.equal(card.open, false);
  }
});

test('SSE rerender preserves keyboard focus on a visible summary without stealing body focus', () => {
  const page = fixture([job('first')]);
  page.render();
  page.cards()[0].children[0].focus();
  page.render();
  assert.equal(page.document.activeElement, page.cards()[0].children[0]);
  assert.equal(page.document.activeElement.focusOptions.preventScroll, true);
  assert.equal(page.content.scrollTop, 125);
  const link = descendants(page.cards()[0].children[1]).find(node => node.tag === 'a');
  link.focus();
  page.render();
  assert.equal(page.document.activeElement, link, 'Only the job summary receives focus restoration');
});
