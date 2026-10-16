const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const tabsCode = app.slice(app.indexOf('const workspacePanels ='), app.indexOf('const progressHistory ='));
const workflowCode = app.slice(app.indexOf('async function selectMedia('), app.indexOf('function badgeClasses('));

function fixture({ mobile = true } = {}) {
  const nodes = new Map();
  const errors = [];
  const views = [];
  const folders = [];
  const requests = [];
  const media = { matches: mobile, addEventListener(name, callback) { this.listener = callback; } };
  const state = { workspaceTab: 'library', folder: 'shows', media: null, targets: new Set(['en']), openReviews: new Set(['job:en']) };
  const failures = { media: false, submit: false };
  let focused = null;
  const get = id => {
    if (!nodes.has(id)) {
      nodes.set(id, {
        id, attributes: {}, dataset: {}, hidden: false, tabIndex: -1, value: '', checked: false, children: [], disabled: false,
        setAttribute(name, value) { this.attributes[name] = value; },
        focus() { focused = id; },
        replaceChildren() { this.children = []; },
        append(item) { this.children.push(item); },
      });
    }
    return nodes.get(id);
  };
  for (const [id, value] of [['model-select', 'test-model'], ['source-language', 'ja'], ['whisper-select', 'medium'], ['audio-track', '1']]) get(id).value = value;
  get('transcript').checked = true;
  const context = vm.createContext({
    state, $: get, folderRequestSequence: 0, mediaRequestSequence: 0,
    matchMedia(query) { assert.equal(query, '(max-width: 1024px)'); return media; },
    Option: class { constructor(text, value) { this.text = text; this.value = value; } },
    seconds: () => '0:24:00',
    displayError: message => errors.push(message),
    thinkingValue: () => null,
    loadFolder: folder => { folders.push(folder); return Promise.resolve(); },
    changeJobsView: async options => views.push(JSON.parse(JSON.stringify(options))),
    api: async (url, options) => {
      requests.push({ url, options });
      if (url.startsWith('/api/media')) {
        if (failures.media) throw Error('Media unavailable');
        return { path: 'shows/episode.mkv', duration: 1440, tracks: [{ index: 1, language: 'ja', codec: 'aac', channels: 2 }] };
      }
      if (failures.submit) throw Error('Submission unavailable');
      return { id: 'new-job' };
    },
  });
  vm.runInContext(tabsCode + '\n' + workflowCode + '\ninitializeWorkspaceTabs();', context);
  return {
    state, nodes, get, errors, views, folders, requests, failures,
    focus: () => focused,
    run: code => vm.runInContext(code, context),
    resize(value) { media.matches = value; media.listener({ matches: value }); },
    select: () => context.selectMedia('shows/episode.mkv'),
    submit: () => context.submitJob(),
  };
}

test('mobile tabs expose one labelled panel and one keyboard tab stop', () => {
  const page = fixture();
  assert.equal(page.get('workspace-tabs').hidden, false);
  for (const name of ['library', 'setup', 'jobs']) {
    const selected = name === 'library';
    const tab = page.get(`tab-${name}`);
    const panel = page.get(`panel-${name}`);
    assert.equal(tab.attributes['aria-selected'], String(selected));
    assert.equal(tab.tabIndex, selected ? 0 : -1);
    assert.equal(panel.hidden, !selected);
    assert.equal(panel.attributes.role, 'tabpanel');
    assert.equal(panel.attributes['aria-labelledby'], `tab-${name}`);
  }
  page.get('tab-jobs').onclick();
  assert.equal(page.state.workspaceTab, 'jobs');
  assert.equal(page.get('panel-jobs').hidden, false);
  assert.equal(page.get('panel-library').hidden, true);
  assert.deepEqual([...page.state.openReviews], ['job:en']);
});

test('arrows, Home and End activate and focus tabs with wrapping', () => {
  const page = fixture();
  const cases = [
    ['library', 'ArrowRight', 'setup'], ['setup', 'ArrowDown', 'jobs'],
    ['jobs', 'ArrowRight', 'library'], ['library', 'ArrowLeft', 'jobs'],
    ['jobs', 'Home', 'library'], ['library', 'End', 'jobs'], ['jobs', 'ArrowUp', 'setup'],
  ];
  for (const [name, key, target] of cases) {
    let prevented = false;
    page.get(`tab-${name}`).onkeydown({ key, preventDefault() { prevented = true; } });
    assert.equal(prevented, true);
    assert.equal(page.state.workspaceTab, target);
    assert.equal(page.focus(), `tab-${target}`);
    assert.equal(page.get(`tab-${target}`).tabIndex, 0);
  }
});

test('desktop shows all regions and returning to mobile preserves the selected tab', () => {
  const page = fixture();
  page.get('tab-jobs').onclick();
  page.resize(false);
  assert.equal(page.get('workspace-tabs').hidden, true);
  for (const name of ['library', 'setup', 'jobs']) {
    assert.equal(page.get(`panel-${name}`).hidden, false);
    assert.equal(page.get(`panel-${name}`).attributes.role, 'region');
    assert.equal(page.get(`panel-${name}`).attributes['aria-labelledby'], `${name}-title`);
    assert.equal(page.get(`tab-${name}`).tabIndex, -1);
  }
  page.resize(true);
  assert.equal(page.state.workspaceTab, 'jobs');
  assert.equal(page.get('panel-jobs').hidden, false);
  assert.equal(page.get('panel-setup').hidden, true);
});

test('successful mobile media selection opens Setup without losing the selected media', async () => {
  const page = fixture();
  await page.select();
  assert.equal(page.state.workspaceTab, 'setup');
  assert.equal(page.state.media.path, 'shows/episode.mkv');
  assert.equal(page.get('media-name').textContent, 'episode.mkv');
  assert.equal(page.get('media-empty').hidden, true);
  assert.equal(page.get('media-options').hidden, false);
  assert.equal(page.get('audio-track').children.length, 1);
  assert.deepEqual(page.folders, ['shows']);
});

test('successful mobile submission opens Jobs and requests All on page one', async () => {
  const page = fixture();
  await page.select();
  await page.submit();
  assert.equal(page.state.workspaceTab, 'jobs');
  assert.deepEqual(page.views, [{ filter: 'all', page: 1 }]);
  const submission = page.requests.find(request => request.url === '/api/jobs');
  const body = JSON.parse(submission.options.body);
  assert.equal(body.path, 'shows/episode.mkv');
  assert.deepEqual(body.targets, ['en']);
  assert.equal(body.review_enabled, false);
  assert.equal(page.get('submit-job').disabled, false);
  assert.equal(page.state.media.path, 'shows/episode.mkv');
});

test('failed actions retain the current tab and desktop actions do not change tab selection', async () => {
  const failed = fixture();
  failed.failures.media = true;
  await failed.select();
  assert.equal(failed.state.workspaceTab, 'library');
  assert.equal(failed.errors.at(-1), 'Media unavailable');
  failed.failures.media = false;
  await failed.select();
  failed.failures.submit = true;
  await failed.submit();
  assert.equal(failed.state.workspaceTab, 'setup');
  assert.equal(failed.errors.at(-1), 'Submission unavailable');
  assert.equal(failed.views.length, 0);
  const desktop = fixture({ mobile: false });
  await desktop.select();
  await desktop.submit();
  assert.equal(desktop.state.workspaceTab, 'library');
  assert.equal(desktop.get('panel-setup').hidden, false);
  assert.deepEqual(desktop.views, [{ filter: 'all', page: 1 }]);
});
