const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const app = fs.readFileSync(path.join(__dirname, 'web', 'app.js'), 'utf8');
const start = app.indexOf('const progressHistory = new Map();');
const end = app.indexOf('async function api(');
assert.ok(start >= 0 && end > start, 'Animation helpers must be present');
const helperCode = app.slice(start, end);

function fixture({ reduced = false, supportsAnimate = true } = {}) {
  const listeners = new Map();
  const media = {
    matches: reduced,
    addEventListener(name, callback) { listeners.set(name, callback); },
  };
  const button = {
    disabled: false,
    attributes: {},
    classes: new Set(),
    setAttribute(name, value) { this.attributes[name] = value; },
    classList: {
      add(value) { button.classes.add(value); },
      remove(value) { button.classes.delete(value); },
    },
  };
  const context = vm.createContext({
    $: () => button,
    matchMedia: () => media,
    element(tag, className = '') {
      const node = {
        tag, className, children: [], attributes: {}, dataset: {}, style: {}, animations: [],
        append(item) { this.children.push(item); },
        setAttribute(name, value) { this.attributes[name] = value; },
      };
      if (supportsAnimate) {
        node.animate = (frames, options) => {
          const animation = {
            frames: JSON.parse(JSON.stringify(frames)),
            options: JSON.parse(JSON.stringify(options)),
            progress: 0, cancelled: false,
            effect: { getComputedTiming: () => ({ progress: animation.progress }) },
            cancel() { this.cancelled = true; },
          };
          node.animations.push(animation);
          return animation;
        };
      }
      return node;
    },
  });
  vm.runInContext(helperCode, context);
  return {
    button,
    render: (...args) => context.renderProgress(...args),
    refresh: callback => context.refreshButton('refresh-jobs', callback),
    reduce(value) {
      media.matches = value;
      listeners.get('change')({ matches: value });
    },
  };
}

test('progress clamps invalid and out-of-range values for visual and accessible output', () => {
  const page = fixture();
  for (const [value, expected] of [[-10, 0], [150, 100], [NaN, 0], [Infinity, 0], ['32.5', 32.5], [null, 0]]) {
    const track = page.render(`job:${String(value)}`, value, 'running', 'Transcription');
    assert.equal(track.children[0].style.width, `${expected}%`);
    assert.equal(track.attributes['aria-valuenow'], String(expected));
    assert.equal(track.attributes.role, 'progressbar');
    assert.equal(track.attributes['aria-valuemin'], '0');
    assert.equal(track.attributes['aria-valuemax'], '100');
  }
});

test('unchanged SSE snapshots reuse the same nodes and do not restart their current tween', () => {
  const page = fixture();
  const initial = page.render('job:transcription', 40, 'running', 'Transcription');
  const updated = page.render('job:transcription', 70, 'running', 'Transcription');
  const fill = updated.children[0];
  const same = page.render('job:transcription', 70, 'running', 'Updated label', 'mt-2 h-[4px]');
  assert.equal(initial, updated);
  assert.equal(updated, same);
  assert.equal(same.children.length, 1);
  assert.equal(fill.animations.length, 1);
  assert.equal(fill.animations[0].cancelled, false);
  assert.equal(same.attributes['aria-label'], 'Updated label');
  assert.equal(same.className, 'progress-track mt-2 h-[4px]');
  assert.deepEqual(fill.animations[0].frames, [{ width: '40%' }, { width: '70%' }]);
  assert.deepEqual(fill.animations[0].options, { duration: 400, easing: 'ease-out' });
});

test('frequent updates continue from the displayed intermediate percentage', () => {
  const page = fixture();
  page.render('job:translation:en', 40, 'running', 'English translation');
  const fill = page.render('job:translation:en', 80, 'running', 'English translation').children[0];
  fill.animations[0].progress = 0.25;
  page.render('job:translation:en', 90, 'running', 'English translation');
  assert.equal(fill.animations[0].cancelled, true);
  assert.deepEqual(fill.animations[1].frames, [{ width: '50%' }, { width: '90%' }]);
  assert.equal(fill.style.width, '90%');
});

test('only running and cancelling progress enables the repeating animation', () => {
  const page = fixture();
  for (const status of ['running', 'cancelling', 'queued', 'completed', 'failed', 'cancelled', 'pending']) {
    const fill = page.render(`job:${status}`, 50, status, 'Job progress').children[0];
    assert.equal(fill.dataset.active, String(['running', 'cancelling'].includes(status)));
  }
  const same = page.render('job:running', 50, 'completed', 'Job progress').children[0];
  assert.equal(same.dataset.active, 'false');
});

test('each language and stage retains independent progress history', () => {
  const page = fixture();
  const english = page.render('job:translation:en', 50, 'running', 'English');
  const french = page.render('job:translation:fr', 20, 'running', 'French');
  assert.notEqual(english, french);
  page.render('job:translation:fr', 40, 'running', 'French');
  assert.equal(english.children[0].animations.length, 0);
  assert.deepEqual(french.children[0].animations[0].frames, [{ width: '20%' }, { width: '40%' }]);
});

test('reduced motion disables tweens and stops an existing tween when the preference changes', () => {
  const reduced = fixture({ reduced: true });
  reduced.render('job:job', 10, 'running', 'Job');
  const staticFill = reduced.render('job:job', 60, 'running', 'Job').children[0];
  assert.equal(staticFill.animations.length, 0);
  assert.equal(staticFill.style.width, '60%');

  const page = fixture();
  page.render('job:review', 20, 'running', 'Review');
  const fill = page.render('job:review', 40, 'running', 'Review').children[0];
  page.reduce(true);
  assert.equal(fill.animations[0].cancelled, true);
  page.render('job:review', 60, 'running', 'Review');
  assert.equal(fill.animations.length, 1);
  assert.equal(fill.style.width, '60%');
});

test('browsers without the animation API still display accurate progress', () => {
  const page = fixture({ supportsAnimate: false });
  page.render('job:job', 10, 'running', 'Job');
  const track = page.render('job:job', 80, 'running', 'Job');
  assert.equal(track.children[0].style.width, '80%');
  assert.equal(track.attributes['aria-valuenow'], '80');
});

test('refresh indicators prevent duplicate requests and clear even after a failed refresh', async () => {
  const page = fixture();
  let resolve;
  const pending = page.refresh(() => new Promise(done => { resolve = done; }));
  assert.equal(page.button.disabled, true);
  assert.equal(page.button.attributes['aria-busy'], 'true');
  assert.equal(page.button.classes.has('is-refreshing'), true);
  await page.refresh(() => assert.fail('A duplicate refresh must not run'));
  resolve();
  await pending;
  assert.equal(page.button.disabled, false);
  assert.equal(page.button.attributes['aria-busy'], 'false');
  assert.equal(page.button.classes.has('is-refreshing'), false);
  await assert.rejects(page.refresh(async () => { throw Error('Refresh failed'); }), /Refresh failed/);
  assert.equal(page.button.disabled, false);
  assert.equal(page.button.attributes['aria-busy'], 'false');
  assert.equal(page.button.classes.has('is-refreshing'), false);
});
