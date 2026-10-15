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
    const assertViewport = async label => {
      await page.waitForFunction(() => document.documentElement.scrollHeight <= innerHeight && document.body.scrollHeight <= innerHeight, null, { timeout: 2000 });
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const sizes = await page.evaluate(() => {
        const workspace = document.querySelector('#workspace-scroll');
        return { height: innerHeight, width: innerWidth, documentHeight: document.documentElement.scrollHeight, documentWidth: document.documentElement.scrollWidth, bodyHeight: document.body.scrollHeight, scrollY, workspaceWidth: workspace.clientWidth, workspaceScrollWidth: workspace.scrollWidth };
      });
      assert(sizes.documentHeight <= sizes.height && sizes.bodyHeight <= sizes.height, `${label}: document stays inside the viewport ${JSON.stringify(sizes)}`);
      assert.equal(sizes.documentWidth, sizes.width, `${label}: no document horizontal scrolling`);
      assert.equal(sizes.scrollY, 0, `${label}: document never scrolls`);
      assert(sizes.workspaceScrollWidth <= sizes.workspaceWidth, `${label}: workspace has no clipped horizontal content`);
    };
    const assertToggleGlyph = async () => {
      const visible = await page.locator('#sidebar-collapse').evaluate(button => {
        const svg = button.querySelector('svg').getBoundingClientRect(), path = button.querySelector('path').getBoundingClientRect();
        return path.width > 0 && path.height > 0 && path.left >= svg.left - 1 && path.right <= svg.right + 1 && path.top >= svg.top - 1 && path.bottom <= svg.bottom + 1;
      });
      assert(visible, 'toggle glyph stays inside its SVG when expanded or collapsed');
    };
    const errors = [], writes = [];
    page.on('pageerror', error => errors.push(error.message));
    const now = Math.floor(Date.now() / 1000);
    const roots = [{ id: 'movies', label: 'Movies', path: '/media/movies' }, { id: 'tv', label: 'Television', path: '/media/tv' }];
    let items = [
      { name: 'Example movie.mkv', path: '/media/movies/Example movie.mkv', root_id: 'movies', type: 'video', tracks: [{ id: 3, language: 'en', codec: 'SubStationAlpha' }, { id: 4, language: 'fr', codec: 'PGS', image: true }], processing: { failed: true, last_error: 'Track 3: output has an invalid timestamp', pending_ocr: true } },
      { name: 'Example show S01E01.mkv', path: '/media/tv/Example show S01E01.mkv', root_id: 'tv', type: 'video', tracks: [{ id: 2, language: 'en', codec: 'SRT' }], processing: { generated_srt_count: 1 } }
    ];
    const jobs = [
      { id: 'failed-1', kind: 'extract', status: 'completed_with_errors', created: now, finished: now, paths: [items[0].path], summary: { failed: 1 }, file_errors: [{ message: items[0].processing.last_error, count: 1, examples: [items[0].name] }], failed_paths: [items[0].path] },
      { id: 'executors-1', kind: 'extract', status: 'completed', created: now - 30, finished: now - 30, paths: ['/media/movies/executors-fixture.mkv'], summary: { converted: 6 }, executors: [{ node_name: 'Local', resource: 'cpu', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }, { node_name: 'Local', resource: 'cpu_ocr', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }, { node_name: 'Local', resource: 'cpu', execution: 'local' }, { node_name: 'Studio GPU', resource: 'gpu', execution: 'remote' }], executors_truncated: true },
      { id: 'backup-1', kind: 'backup', status: 'completed', created: now - 60, summary: {} }
    ];
    jobs[0].executors=[null,{resource:'unknown',node_name:'Invalid',execution:'remote'},{resource:'cpu',node_name:'',execution:'local'}];
    let inventorySource = 'filesystem', inventoryAuthority = { status: 'ready' }, revision = 1;
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
      else if (endpoint === '/api/status') value = { edition: 'cuda', version: 'fixture', roots, running: null, queue: [], history: jobs, logs: [{ time: now, level: 'INFO', message: 'Fixture operation completed' }], inventory: { updated: now, state_revision: revision, partial: true, source: inventorySource, authority: inventoryAuthority }, pipeline: { root_count: 2, files_inspected: 2, pending_ocr_files: 37, failed_files: 512, generated_srt_count: 11326, unreachable_roots: 0, inventory_truncated: true } };
      else if (endpoint === '/api/inventory') value = { items, updated: now, state_revision: revision, truncated: inventorySource !== 'bazarr', source: inventorySource, authority: inventoryAuthority };
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
    await page.setViewportSize({ width: 1440, height: 640 });
    await assertViewport('short desktop');
    const headerBefore = await page.locator('.topbar').boundingBox(), footerBefore = await page.locator('.sidebar-bottom').boundingBox();
    await page.locator('#workspace-scroll').focus();
    await page.keyboard.press('PageDown');
    await page.waitForFunction(() => document.querySelector('#workspace-scroll').scrollTop > 0);
    await assertViewport('desktop after keyboard scrolling');
    assert.deepEqual(await page.locator('.topbar').boundingBox(), headerBefore, 'header stays fixed while the workspace scrolls');
    assert.deepEqual(await page.locator('.sidebar-bottom').boundingBox(), footerBefore, 'navigation footer stays fixed');
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    assert.equal(await page.locator('#workspace-scroll').evaluate(el => el.scrollTop), 0, 'navigation resets the workspace scroll position');
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    await page.setViewportSize({ width: 1440, height: 1000 });
    await assertToggleGlyph();
    await page.locator('#sidebar-collapse').click();
    assert.equal(await page.locator('#sidebar-collapse').getAttribute('aria-expanded'), 'false');
    assert(await page.locator('#sidebar-collapse').isVisible(), 'collapsed navigation keeps its top-bar toggle');
    await assertToggleGlyph();
    if (output) await page.screenshot({ path: path.join(output, 'sidebar-collapsed.png'), style: '.toast { visibility: hidden; }' });
    await page.reload();
    await page.locator('#pipeline-ready').getByText('11,326', { exact: true }).waitFor();
    assert.equal(await page.locator('#sidebar-collapse').getAttribute('aria-expanded'), 'false', 'collapsed state survives reload');
    await assertToggleGlyph();
    await page.locator('#sidebar-collapse').click();
    await assertToggleGlyph();
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
    const executorCard=page.locator('#jobs-list .job-card').filter({ hasText: 'executors-fixture.mkv' });
    assert.equal(await executorCard.count(),1,'persisted activity includes the completed multiworker job');
    assert.deepEqual(await executorCard.locator('.job-executors > li:not(.job-executors-more):not(.job-executors-truncated)').allTextContents(),['CPU · Local','GPU OCR · Studio GPU (remote)','CPU OCR · Local','GPU OCR · Studio GPU (remote)']);
    assert.equal(await executorCard.locator('.job-executors-more summary').textContent(),'2 more workers');
    await executorCard.locator('.job-executors-more summary').click();
    jobs[1].summary.converted++;
    await page.locator('#refresh-button').click();
    assert.equal(await executorCard.locator('.job-executors-more details').evaluate(details=>details.open),true,'worker disclosure remains open after a status update');
    assert.deepEqual(await executorCard.locator('.job-executors-more ul li').allTextContents(),['CPU · Local','GPU OCR · Studio GPU (remote)']);
    assert.match(await executorCard.locator('.job-executors').innerText(),/Additional workers not shown/);
    const legacyCard=page.locator('#jobs-list .job-card').filter({ hasText: 'Example movie.mkv' });
    assert.equal(await legacyCard.locator('.job-executors').count(),0,'legacy history does not invent executor hardware');
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
      await assertViewport(`library ${width}px`);
    }
    // Bazarr requirements take precedence over historical failures and wildcard tracks.
    const target = (language, forced = false, hi = false) => ({ language, forced, hi });
    const wanted = [target('en'), target('fr'), target('ar')];
    const covered = {
      name: 'Covered movie.mkv', path: '/media/movies/Covered movie.mkv', root_id: 'movies', type: 'video',
      wanted_targets: wanted, missing_targets: [], missing_srt: false,
      coverage: { status: 'covered', reason: 'Valid SRT already covers every wanted language', existing_targets: wanted, missing_targets: [] },
      tracks: [{ id: 3, language: 'en', codec: 'PGS', image: true }, ...Array.from({ length: 27 }, (_, i) => ({ id: i + 4, language: 'und', codec: 'PGS', image: true }))],
      processing: { failed: true, pending_ocr: true, last_error: 'Historical extraction failure' }
    };
    const needed = {
      name: 'Missing French.mkv', path: '/media/movies/Missing French.mkv', root_id: 'movies', type: 'video',
      wanted_targets: wanted, missing_targets: [target('fr')], missing_srt: true,
      coverage: { status: 'needs_conversion', reason: 'French SRT is missing', existing_targets: [target('en'), target('ar')], missing_targets: [target('fr')] },
      tracks: [{ id: 2, language: 'en', codec: 'SRT' }, { id: 3, language: 'fr', codec: 'ASS' }, { id: 4, language: 'fr', codec: 'ASS', forced: true }, { id: 5, language: 'und', codec: 'PGS', image: true }]
    };
    items = [covered, needed]; inventorySource = 'bazarr'; revision++;
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-source').getByText('Bazarr', { exact: true }).waitFor();
    await page.locator('#inventory-filter').selectOption('covered');
    assert.equal(await page.locator('#inventory-body tr').count(), 1);
    assert.match(await page.locator('#inventory-body').innerText(), /Already covered/);
    assert.doesNotMatch(await page.locator('#inventory-body').innerText(), /Historical extraction failure/);
    await page.locator('#inventory-body .row-action').first().click();
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 0);
    assert.match(await page.locator('#track-drawer-hint').innerText(), /No extraction needed/);
    assert(await page.locator('#convert-selected-tracks').isDisabled());
    assert(await page.locator('#track-drawer-list input').first().isDisabled());
    await page.locator('#force-covered-tracks').check();
    await page.locator('#track-drawer-list input').first().check();
    await page.locator('#convert-selected-tracks').click();
    await page.locator('#track-drawer-backdrop').waitFor({ state: 'hidden' });
    assert.equal(writes.at(-1).body.force, true);
    assert.deepEqual(writes.at(-1).body.track_ids, [3], 'force targets one deliberate track');
    await page.locator('#inventory-filter').selectOption('needed');
    await page.locator('#inventory-body .row-action').first().click();
    // One-job English selection from earlier must be reset to profile requirements.
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 0);
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    await page.locator('[data-open-languages="quick"]').last().click();
    await page.locator('.all-option input').check();
    await page.locator('#apply-languages').click();
    await page.getByRole('button', { name: 'Library', exact: true }).click();
    await page.locator('#inventory-body .row-action').first().click();
    assert.equal(await page.locator('#track-drawer-list input:checked').count(), 1, 'only missing full French target selected');
    assert(await page.locator('#track-drawer-list input').nth(1).isChecked());
    assert(!(await page.locator('#track-drawer-list input').nth(2).isChecked()), 'forced French does not cover full French');
    assert(!(await page.locator('#track-drawer-list input').nth(3).isChecked()), 'untagged track not selected automatically');
    await page.getByRole('button', { name: 'Close track details' }).click();
    await page.locator('#inventory-filter').selectOption('all');
    if (output) await page.screenshot({ path: path.join(output, 'bazarr-library-desktop.png'), fullPage: true, style: '.toast { visibility: hidden; }' });
    inventoryAuthority = { status: 'refresh_pending' }; revision++;
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-authority').getByText(/indexing or inventory refresh is still pending/).waitFor();
    assert(await page.locator('[data-action="scan"]').first().isDisabled());
    assert(await page.locator('[data-action="inspect"]').first().isEnabled(), 'sync remains available');
    inventoryAuthority = { status: 'ready' }; revision++;
    needed.missing_targets = []; needed.missing_srt = false;
    needed.coverage = { status: 'covered', reason: 'Bazarr confirms the new SRT', existing_targets: wanted, missing_targets: [] };
    await page.locator('#refresh-button').click();
    await page.locator('#inventory-filter').selectOption('covered');
    await page.waitForFunction(() => document.querySelectorAll('#inventory-body tr').length === 2);
    assert(await page.locator('[data-action="scan"]').first().isEnabled());
    await page.getByRole('button', { name: 'Overview', exact: true }).click();
    await page.locator('#quick-form').getByRole('button', { name: 'Review conversion' }).click();
    assert.match(await page.locator('#conversion-scope').innerText(), /Bazarr profile/);
    await page.locator('#confirm-conversion').click();
    await page.locator('#conversion-modal').waitFor({ state: 'hidden' });
    assert.equal(writes.at(-1).body.include_ocr, true, 'manual conversion requests available OCR slots');
    // A timed-out rescan is uncertain, while the actual conversion is complete.
    const pendingNotice = { provider: 'bazarr', ok: false, pending: true, uncertain: true, message: 'Bazarr response timed out; scan completion not yet confirmed' };
    jobs.unshift({ id: 'index-pending', kind: 'extract', status: 'completed', created: now, finished: now, summary: { converted: 1 }, notifications: [pendingNotice] });
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    await page.locator('#activity-kind').selectOption('all');
    await page.locator('#refresh-button').click();
    const pendingCard = page.locator('#jobs-list .job-card').first();
    await pendingCard.getByText('Converted · indexing pending', { exact: true }).waitFor();
    assert.equal(await pendingCard.locator('.job-error').count(), 0, 'pending confirmation is not a reported conversion error');
    assert(await pendingCard.locator('.result-pending').isVisible());
    const writesBeforeCheck = writes.length;
    await pendingCard.getByRole('button', { name: 'Check Bazarr indexing' }).click();
    assert.equal(writes.length, writesBeforeCheck + 1);
    assert.equal(writes.at(-1).endpoint, '/api/jobs');
    assert.equal(writes.at(-1).body.kind, 'inspect', 'verification queues an inventory read, not another extraction or rescan');
    for (const width of [1440, 320]) {
      await page.setViewportSize({ width, height: 640 });
      await pendingCard.scrollIntoViewIfNeeded();
      await assertViewport(`pending indexing ${width}px`);
      assert(await pendingCard.getByRole('button', { name: 'Check Bazarr indexing' }).isVisible());
      if (output) await page.screenshot({ path: path.join(output, `bazarr-indexing-pending-${width}.png`), style: '.toast { visibility: hidden; }' });
    }
    pendingNotice.ok = true; pendingNotice.pending = false; pendingNotice.uncertain = false;
    pendingNotice.message = 'Published SRT files confirmed by a later Bazarr inventory sync';
    await page.locator('#refresh-button').click();
    await pendingCard.getByText(pendingNotice.message, { exact: true }).waitFor();
    assert.equal(await pendingCard.locator('.result-pending, .job-error').count(), 0);
    await pendingCard.getByText('Completed', { exact: true }).waitFor();
    // Long activity and native controls must remain usable inside the fixed shell.
    await page.setViewportSize({ width: 1440, height: 1000 });
    jobs.push(...Array.from({ length: 20 }, (_, i) => ({ id: `history-${i}`, kind: 'inspect', status: 'completed', created: now - i * 60, summary: { inspected: 1 } })));
    await page.getByRole('button', { name: 'Activity', exact: true }).click();
    await page.locator('#activity-kind').selectOption('all');
    await page.locator('#refresh-button').click();
    await page.locator('#jobs-list .job-card').nth(20).waitFor();
    for (const [width, height] of [[1920, 1080], [1024, 600], [390, 600], [320, 480]]) {
      await page.setViewportSize({ width, height });
      await assertViewport(`activity resized to ${width}x${height}`);
      const header = await page.locator('.topbar').boundingBox();
      await page.locator('#jobs-list .job-card').last().scrollIntoViewIfNeeded();
      await assertViewport(`long activity ${width}x${height}`);
      assert.deepEqual(await page.locator('.topbar').boundingBox(), header, 'scrolling long activity keeps the header visible');
      const last = await page.locator('#jobs-list .job-card').last().boundingBox();
      assert(last.y + last.height <= height + 1, 'the final activity row remains reachable');
      if (width===320) {
        const executorList=executorCard.locator('.job-executors');
        await executorList.scrollIntoViewIfNeeded();
        assert(await executorList.isVisible());
        assert.equal(await executorList.evaluate(el=>el.scrollWidth===el.clientWidth),true,'executor labels wrap without horizontal overflow at 320px');
        if(output)await executorCard.screenshot({ path: path.join(output,'executor-workers-mobile.png'), style: '.toast { visibility: hidden; }' });
      }
    }
    await page.setViewportSize({ width: 1440, height: 640 });
    await page.locator('#activity-status').focus();
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowDown');
    assert.equal(await page.locator('#activity-status').inputValue(), 'running', 'select retains native keyboard selection');
    await page.locator('#activity-status').selectOption('all');
    const selectStyles = await page.locator('#activity-status').evaluate(select => {
      const css = getComputedStyle(select), option = getComputedStyle(select.options[0]);
      return { rightPadding: parseFloat(css.paddingRight), arrow: css.backgroundImage, optionBackground: option.backgroundColor, background: css.backgroundColor };
    });
    assert(selectStyles.rightPadding >= 32 && selectStyles.arrow !== 'none', 'select text has reserved space for a visible arrow');
    assert.notEqual(selectStyles.optionBackground, 'rgb(255, 255, 255)', 'select options use the dark palette');
    if (output) await page.screenshot({ path: path.join(output, 'activity-fixed-viewport.png'), style: '.toast { visibility: hidden; }' });
    assert.deepEqual(errors, [], 'browser must have no uncaught errors');
    console.log('PASS Subforge fixed viewport/internal scrolling, visible collapsed toggle, native selects, desktop/mobile UI, language isolation, scope review, failure triage, workers, remote nodes, executor metadata, keyboard, Bazarr coverage/authority, and pending indexing confirmation regressions');
    await context.close();
  } finally {
    if (browser) await browser.close();
    await new Promise(resolve => server.close(resolve));
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
