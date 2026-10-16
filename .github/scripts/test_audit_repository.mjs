// Disposable fixtures contain no real credentials or deployment data.
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { auditMarkdown, auditEnvExample, auditGlanceYaml, auditRepository, main } from './audit_repository.mjs';

const rules = findings => new Set(findings.map(item => item.rule));

test('private ranges rejected; loopback and documentation addresses allowed', () => {
  const findings = auditMarkdown('README.md', '10.1.2.3\n172.16.1.2\n192.168.1.2\n');
  assert.deepEqual(findings.map(item => item.line), [1, 2, 3]);
  assert.deepEqual(auditMarkdown('README.md', 'http://localhost:7575\n127.0.0.1\n192.0.2.10\n172.15.1.2\n'), []);
});

test('credential assignments and private key markers rejected without values', () => {
  const marker = '-----BEGIN OPENSSH ' + 'PRIVATE KEY-----';
  const findings = auditMarkdown('README.md', 'API_KEY=fixture-sensitive-value\n$env:MY_PASSWORD="fixture-sensitive-value"\nAuthorization: Bearer fixture-sensitive-value\n' + marker + '\n');
  assert.equal(findings.length, 4);
  assert.ok(rules(findings).has('markdown-private-key'));
  assert.ok(!JSON.stringify(findings).includes('fixture-sensitive-value'));
});

test('placeholders and local dummy keys allowed without permitting suffixes', () => {
  const text = [
    'API_KEY=', 'API_KEY=""', 'API_KEY=${MY_API_KEY}', 'API_KEY=<your-api-key>',
    'TOKEN=your_token_here', 'PASSWORD=example-password', 'OPENAI_API_KEY=lmstudio',
    'Authorization: Bearer ${MY_API_KEY}',
  ].join('\n');
  assert.deepEqual(auditMarkdown('README.md', text), []);
  assert.ok(auditMarkdown('README.md', 'TOKEN="${MY_TOKEN}fixture-sensitive-value"').length);
});

test('example credentials must be blank, including placeholders', () => {
  const safe = 'GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=""\nYT_TA_USERNAME=\n';
  assert.deepEqual(auditEnvExample(safe), []);
  const findings = auditEnvExample(safe + 'API_KEY=your_key_here\nPASSWORD="fixture-sensitive-value"\n');
  assert.equal(findings.length, 2);
  assert.deepEqual(rules(findings), new Set(['example-credential-not-blank']));
});

test('example Docker endpoint requires the monitoring proxy', () => {
  assert.ok(rules(auditEnvExample('GLANCE_DOCKER_HOST=tcp://localhost:2375\n')).has('example-docker-client-not-proxy'));
  assert.ok(rules(auditEnvExample('API_KEY=\n')).has('example-docker-proxy-setting-missing'));
});

test('hash-prefixed credentials and JSON assignments rejected', () => {
  const example = 'GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=#fixture-sensitive-value\n';
  assert.ok(rules(auditEnvExample(example)).has('example-credential-not-blank'));
  assert.ok(rules(auditMarkdown('README.md', '{"API_KEY": "fixture-sensitive-value"}')).has('markdown-credential-assignment'));
});

test('Glance environment endpoints, templates, and relative assets allowed', () => {
  const text = [
    'url: ${GLANCE_GITHUB_URL}/search?q={QUERY}', 'sock-path: ${GLANCE_DOCKER_HOST}',
    'icon: /assets/subtitle-studio.svg', '  <a href="${HOMELAB_URL}:5055/requests">Requests</a>',
    '  <img href="/assets/example.svg">',
  ].join('\n');
  assert.deepEqual(auditGlanceYaml('glance/config/pages/tools.yml', text), []);
});

test('Glance literal URLs, addresses, domains, and wrong proxy rejected', () => {
  const text = [
    'url: https://example.invalid/', 'check-url: 192.0.2.10', 'url: example.invalid/feed',
    'address: "::1"', 'sock-path: ${OTHER_DOCKER_HOST}', '  <a href="example.invalid/path">Link</a>',
  ].join('\n');
  assert.deepEqual(rules(auditGlanceYaml('glance/config/pages/tools.yml', text)), new Set([
    'glance-literal-url', 'glance-literal-ip', 'glance-endpoint-not-env',
    'glance-docker-client-not-env-proxy', 'glance-href-not-env',
  ]));
});

test('quoted YAML keys and flow mapping endpoints checked', () => {
  const safe = '{"url": "${HOMELAB_URL}", "sock-path": "${GLANCE_DOCKER_HOST}"}';
  assert.deepEqual(auditGlanceYaml('glance/config/glance.yml', safe), []);
  const findings = auditGlanceYaml('glance/config/glance.yml', '{url: ${HOMELAB_URL}, "check-url": "example.invalid"}');
  assert.ok(rules(findings).has('glance-endpoint-not-env'));
});

function repositoryFixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'homelab-audit-'));
  t.after(() => {
    // Remove only the exact disposable directory created for this test.
    assert.equal(dirname(resolve(root)), resolve(tmpdir()));
    assert.ok(basename(root).startsWith('homelab-audit-'));
    rmSync(root, { recursive: true, force: true });
  });
  const git = (...args) => {
    const result = spawnSync('git', ['-C', root, ...args], { encoding: 'utf8', windowsHide: true });
    assert.equal(result.status, 0, 'Git fixture operation failed');
  };
  const write = (name, text) => {
    const path = join(root, name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, text, 'utf8');
  };
  git('init', '--quiet');
  write('.gitignore', '.env\n');
  write('.env.example', 'GLANCE_DOCKER_HOST=tcp://docker-proxy:2375\nAPI_KEY=\n');
  write('README.md', '# Example\nUse http://localhost:7575\n');
  git('add', '.gitignore', '.env.example', 'README.md');
  return { root, git, write };
}

test('clean repository and SVG namespace allowed', t => {
  const { root, write } = repositoryFixture(t);
  write('glance/config/glance.yml', 'app-name: ${GLANCE_APP_NAME}\n');
  write('glance/config/assets/icon.svg', '<svg xmlns="http://www.w3.org/2000/svg"/>');
  assert.deepEqual(auditRepository(root), []);
});

test('untracked Glance YAML and Markdown checked', t => {
  const { root, write } = repositoryFixture(t);
  write('glance/config/pages/new.yaml', 'url: https://example.invalid/\n');
  write('AGENTS.md', 'Private server: 10.1.2.3\n');
  const found = rules(auditRepository(root));
  assert.ok(found.has('glance-literal-url'));
  assert.ok(found.has('markdown-private-ip'));
});

test('tracked real env and missing ignore rejected without reading env', t => {
  const { root, git, write } = repositoryFixture(t);
  // Invalid UTF-8 would fail readText, proving .env is never decoded.
  write('.env', Buffer.from([0xff, 0xfe]));
  git('add', '--force', '.env');
  write('.gitignore', '');
  assert.deepEqual(rules(auditRepository(root)), new Set(['real-env-tracked', 'real-env-not-ignored']));
});

test('local env never read or misreported as tracked', t => {
  const { root, write } = repositoryFixture(t);
  write('.env', Buffer.from([0xff, 0xfe]));
  assert.deepEqual(auditRepository(root), []);
  write('.gitignore', '');
  assert.deepEqual(rules(auditRepository(root)), new Set(['real-env-not-ignored']));
});

test('CLI failure exit code and output never display matched values', t => {
  const { root, write } = repositoryFixture(t);
  write('README.md', '# Fixture\nAPI_KEY=fixture-sensitive-value\n');
  const script = fileURLToPath(new URL('./audit_repository.mjs', import.meta.url));
  const result = spawnSync(process.execPath, [script, '--root', root], { encoding: 'utf8', windowsHide: true });
  assert.equal(result.status, 1);
  assert.ok(result.stdout.includes('README.md:2: markdown-credential-assignment'));
  assert.ok(!(result.stdout + result.stderr).includes('fixture-sensitive-value'));
});

test('source policy rejects Python, notebooks, and PowerShell but preserves ignored runtime data', t => {
  const { root, write } = repositoryFixture(t);
  write('.gitignore', '.env\nruntime/\n');
  for (const name of ['helper.py', 'helper.pyw', 'types.pyi', 'analysis.ipynb', 'helper.ps1', 'runtime/plugin.py']) {
    write(name, '');
  }
  const findings = auditRepository(root);
  assert.equal(findings.filter(item => item.rule === 'repository-python-source').length, 4);
  assert.equal(findings.filter(item => item.rule === 'repository-powershell-script').length, 1);
  assert.ok(!findings.some(item => item.path.startsWith('runtime/')));
});

test('invalid encoding fails closed without showing content', t => {
  const { root, write } = repositoryFixture(t);
  write('README.md', Buffer.from([0xff, 0xfe]));
  const output = [];
  assert.equal(main(['--root', root], line => output.push(line)), 2);
  assert.deepEqual(output, ['Repository privacy audit could not read the repository.']);
});

test('invalid UTF-8 Git filenames fail closed for tracked and untracked files', {
  skip: process.platform === 'win32' ? 'Windows filenames use Unicode' : false,
}, t => {
  for (const tracked of [false, true]) {
    const { root, git } = repositoryFixture(t);
    const path = Buffer.concat([Buffer.from(join(root, 'invalid-')), Buffer.from([0xff]), Buffer.from('.md')]);
    writeFileSync(path, 'API_KEY=fixture-sensitive-value\n');
    if (tracked) git('add', '--all');
    const output = [];
    assert.equal(main(['--root', root], line => output.push(line)), 2);
    assert.deepEqual(output, ['Repository privacy audit could not read the repository.']);
  }
});

test('invalid CLI arguments are rejected', () => {
  for (const args of [['--root'], ['--other'], ['--root', '.', 'extra']]) {
    assert.equal(main(args, () => {}), 2);
  }
});
