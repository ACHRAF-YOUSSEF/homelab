'use strict';

// Isolated browser regression: serves production assets and intercepts every API.
// NODE_PATH may point to an installed Playwright package directory.
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');

async function main() {
  const web = path.resolve(__dirname, '../web');
  const output = process.env.SUBFORGE_UI_OUTPUT && path.resolve(process.env.SUBFORGE_UI_OUTPUT);
  if (output) await fs.mkdir(output, { recursive: true });
  const server = http.createServer(async (req, res) => {
    const asset = { '/': 'index.html', '/static/app.js': 'app.js', '/static/style.css': 'style.css', '/static/icon.svg': 'icon.svg' }[req.url];
    if (!asset) { res.writeHead(404); res.end(); return; }
    const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' }[path.extname(asset)];
    res.writeHead(200, { 'Content-Type': mime });
    res.end(await fs.readFile(path.join(web, asset)));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  let browser;
  try {
    browser = await chromium.launch({ headless: true, ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE } : {}) });
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
    const page = await context.newPage();
    const errors = [], writes = [];
    page.on('pageerror', error => errors.push(error.message));
    const now = Math.floor(Date.now() / 1000);
    const roots = [{ id: 'movies', label: 'Movies', path: '/media/movies' }, { id: 'tv', label: 'Television', path: '/media/tv' }];
    const items = [
      { name: 'Example movie.mkv', path: '/media/movies/Example movie.mkv', root_id: 'movies', type: 'video', tracks: [{ id: 3, language: 'en', codec: 'SubStationAlpha' }, { id: 4, language: 'fr', codec: 'PGS', image: true }], processing: { failed: true, last_error: 'Track 3: output has an invalid timestamp', pending_ocr: true } },
      { name: 'Example show S01E01.mkv', path: '/media/tv/Example show S01E01.mkv', root_id: 'tv', type: 'video', tracks: [{ id: 2, language: 'en', codec: 'SRT' }], processing: { generated_srt_count: 1 } }
    ];
    const jobs = [
      { id: 'failed-1', kind: 'extract', status: 'completed_with_errors', created: now, finished: now, paths: [items[0].path], summary: { failed: 1 }, file_errors: [{ message: items[0].processing.last_error, count: 1, examples: [items[0].name] }], failed_paths: [items[0].path] },
      { id: 'backup-1', kind: 'backup', status: 'completed', created: now - 60, summary: {} }
    ];
    let settings = { languages: ['*'], permissions: { ocr: true }, integrations: {} };
    let allocation = { cpu_workers: 4, ocr_workers: 1, gpu_workers: 1, paused: false };
    const remote = { enabled: true, nodes: [{ id: 'node-1', name: 'Studio GPU', resource: 'gpu', last_seen_seconds: 2, active: 1, status: 'busy' }], pending: 0, active: 1 };
    const workerData = () => ({ edition: 'cuda', max_requested: 1000000, allocation, requested: allocation, effective: { cpu_workers: 4, ocr_workers: 1, gpu_workers: 1 }, active: { cpu_workers: 0, ocr_workers: 0, gpu_workers: 0 }, detected: { cpu_allowance_cores: 4, gpu_available: true, gpu_verification: 'verified', gpu_reason: 'CUDA runtime enabled' }, deployment: { max_cpu_workers: 32, max_ocr_workers: 16, max_gpu_workers: 4 }, admission: { gpu: { allowed: false, reason: 'Waiting for memory headroom' } }, workers: [], remote });
    await page.route('**/api/**', async route => {
      const request = route.request(), endpoint = new URL(request.url()).pathname;
      let value;
      if (request.method() !== 'GET') {
        const body = request.postDataJSON(); writes.push({ endpoint, body });
        if (endpoint === '/api/workers') allocation = body;
        if (endpoint === '/api/settings') settings = { ...settings, ...body };
      }
      if (endpoint === '/api/session') value = { csrf_token: 'disposable-ui-fixture' };
      else if (endpoint === '/api/status') value = { edition: 'cuda', version: 'fixture', roots, running: null, queue: [], history: jobs, logs: [{ time: now, level: 'INFO', message: 'Fixture operation completed' }], inventory: { updated: now, state_revision: 1, partial: true }, pipeline: { root_count: 2, files_inspected: 2, pending_ocr_files: 37, failed_files: 512, generated_srt_count: 11326, unreachable_roots: 0, inventory_truncated: true } };
      else if (endpoint === '/api/inventory') value = { items, updated: now, state_revision: 1, truncated: true };
      else if (endpoint === '/api/settings') value = settings;
      else if (endpoint === '/api/workers') value = workerData();
      else if (endpoint === '/api/nodes') value = remote;
      else if (endpoint === '/api/system') value = { edition: 'cuda', version: 'fixture', uptime_seconds: 120, resource_usage: { cpu: { effective_cores: 4 }, memory: { cgroup_current_bytes: 1300000000, cgroup_limit_bytes: 1610612736 } }, resources: {}, limits: {} };
      else if (endpoint === '/api/jobs') value = { message: 'Fixture job queued' };
      else if (endpoint === '/api/root-folders') value = { roots: [], mounts: [] };
      else if (endpoint === '/api/tasks') value = { tasks: [], paused: false };
      else throw new Error(`Unexpected fixture API ${request.method()} ${endpoint}`);
      await route.fulfill({ contentType: 'application/json', body: JSON.stringify(value) });
    });
    await page.goto(`http://127.0.0.1:${server.address().port}/#/overview`);
    await page.getByText('Service connected', { exact: true }).waitFor();
    await page.locator('#pipeline-ready').getByText('11,326', { exact: true }).waitFor();
    assert.equal(writes.length, 0, 'initial page must not mutate the backend');
    await page.locator('#sidebar-collapse').click();
    assert.equal(await page.locator('#sidebar-collapse').getAttribute('aria-expanded'), 'false');
    assert(await page.locator('#sidebar-collapse').isVisible(), 'collapsed navigation keeps its top-bar toggle');
    await page.locator('#sidebar-collapse').click();
    await page.locator('[data-open-languages="quick"]').last().click();
    await page.locator('#language-list input[value="en"]').check();
    assert.equal(await page.locator('.all-option input').isChecked(), false, 'specific selection clears All visibly');
    assert.equal(await page.locator('#language-count').textContent(), '1 language selected');
    await page.locator('#apply-languages').click();
    assert.match(await page.locator('#quick-languages').textContent(), /English/);
    assert.equal(await page.locator('#settings-languages').textContent(), 'All languages', 'one-job choice must not edit defaults');
    await page.locator('#quick-root').selectOption('movies');
    await page.locator('#quick-form').getByRole('button', { name: 'Review conversion' }).click();
    assert.equal(writes.length, 0, 'scope review must not start a job');
    assert.match(await page.locator('#conversion-scope').innerText(), /Movies/);
    await page.locator('#confirm-conversion').click();
    await page.locator('#conversion-modal').waitFor({ state: 'hidden' });
    assert.deepEqual(writes.at(-1).body.root_ids, ['movies']);
    assert.deepEqual(writes.at(-1).body.languages, ['en']);
    await page.locator('[data-attention="failed"]').click();
    assert.match(await page.locator('#inventory-scope-note').innerText(), /512 failed files across/);
    assert.match(await page.locator('#inventory-body').innerText(), /invalid timestamp/);
    await page.locator('#inventory-body .row-action').first().click();
    assert.match(await page.locator('#track-drawer-failure').innerText(), /invalid timestamp/);
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 1, 'track defaults respect selected languages');
    assert.equal(await page.locator('#convert-selected-tracks').textContent(), 'Convert 1 track');
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    assert.doesNotMatch(await page.locator('#jobs-list').innerText(), /State backup/);
    await page.locator('#activity-kind').selectOption('all');
    assert.match(await page.locator('#jobs-list').innerText(), /State backup/);
    await page.getByRole('button', { name: 'Settings', exact: true }).click();
    assert.equal(await page.locator('.settings-tab.active').getAttribute('aria-current'), 'page');
    await page.locator('[data-open-languages="settings"]').click();
    await page.locator('#language-list input[value="fr"]').check();
    await page.locator('#apply-languages').click();
    assert.match(await page.locator('#settings-languages').textContent(), /French/);
    assert.match(await page.locator('#quick-languages').textContent(), /English/, 'defaults must not overwrite one-job scope');
    await page.locator('#save-settings').click();
    await page.getByText('Settings saved.', { exact: true }).waitFor();
    assert.deepEqual(writes.at(-1).body.languages, ['fr']);
    await page.locator('[data-setting-tab="workers"]').click();
    await page.locator('#worker-nodes').getByText('Studio GPU', { exact: true }).waitFor();
    await page.locator('#worker-cpu-count').fill('1000');
    await page.locator('#save-worker-allocation').click();
    await page.getByText('Worker allocation saved. It applies to new files.', { exact: true }).waitFor();
    assert.equal(writes.at(-1).body.cpu_workers, 1000);
    assert.match(await page.locator('#worker-gpu-capacity').textContent(), /memory headroom/);
    if (output) await page.screenshot({ path: path.join(output, 'workers-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    if (output) await page.screenshot({ path: path.join(output, 'overview-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.locator('#sidebar').evaluate(el => el.inert), true);
    const quick = await page.locator('.quick-panel').boundingBox(), recent = await page.locator('.activity-panel').boundingBox();
    assert(quick.y < recent.y, 'mobile conversion precedes history');
    assert.equal(await page.evaluate(() => document.body.scrollWidth), 390, 'mobile page must not overflow');
    if (output) await page.screenshot({ path: path.join(output, 'overview-mobile.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.locator('#mobile-menu').click();
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    assert.equal(await page.locator('#sidebar').evaluate(el => el.inert), true);
    const table = await page.locator('.inventory-table').boundingBox();
    assert(table.width <= 390, 'mobile inventory uses cards without horizontal table overflow');
    assert(await page.locator('#inventory-body .row-action').first().isVisible());
    if (output) await page.screenshot({ path: path.join(output, 'library-mobile.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    await page.locator('#inventory-body tr').first().focus();
    await page.keyboard.press('Enter');
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'visible' });
    await page.keyboard.press('Escape');
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'hidden' });
    assert.equal(await page.locator('#inventory-body tr').first().evaluate(el => document.activeElement === el), true, 'drawer restores keyboard focus');
    for (const width of [320, 768, 1024, 1920]) {
      await page.setViewportSize({ width, height: 900 });
      assert.equal(await page.evaluate(() => document.body.scrollWidth), width, `library fits ${width}px viewport`);
    }
    assert.deepEqual(errors, [], 'browser must have no uncaught errors');
    console.log('PASS Subforge isolated desktop/mobile UI, language isolation, scope review, failure triage, workers, remote-node rendering, and keyboard regressions');
    await context.close();
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
