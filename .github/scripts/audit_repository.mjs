#!/usr/bin/env node
// Check repository privacy invariants without displaying matched values.
import { existsSync, lstatSync, readFileSync, readdirSync } from 'node:fs';
import { isIP } from 'node:net';
import { dirname, join, relative, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const IPV4 = /(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])/g;
const IPV6 = /(?<![\w])(?:[a-fA-F0-9]{0,4}:){2,}[a-fA-F0-9:.]*(?![\w])/g;
const PRIVATE_KEY = /-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----/;
const ASSIGNMENT = /(?<![\w-])(?<name>[A-Za-z_][\w-]*)["']?\s*(?:=|:)\s*/g;
const CREDENTIAL_NAME = /PASSWORD|PASSPHRASE|(?:^|_)TOKEN(?:_|$)|(?:^|_)API_?KEY(?:_|$)|(?:^|_)SECRET(?:_|$)|PRIVATE_KEY|USERNAME|ADMIN_USER|^AUTHORIZATION$/i;
const ENV_ASSIGNMENT = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$/;
const URL_LITERAL = /\b[a-z][a-z0-9+.-]*:\/\//i;
const URL_FIELD = /(?:^\s*(?:-\s+)?|[,{]\s*)(?<quote>["']?)(?<name>url|title-url|check-url|search-engine|sock-path)\k<quote>:\s*/g;
const HREF = /\bhref\s*=\s*(["'])(.*?)\1/gi;
const ENV_PREFIX = /^\$\{[A-Z][A-Z0-9_]*\}/;
const decoder = new TextDecoder('utf-8', { fatal: true });
const finding = (path, line, rule) => ({ path, line, rule });

function literalValue(raw) {
  const value = raw.trim();
  if (!value) return '';
  if ('"\''.includes(value[0])) {
    const end = value.indexOf(value[0], 1);
    return value.slice(1, end < 0 ? undefined : end);
  }
  return value.split(/[`;]|\s+#/, 1)[0].trim();
}

function placeholder(raw) {
  let value = raw;
  if (!value) return true;
  if (/^bearer /i.test(value)) value = value.slice(7).trim();
  return /^(?:\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$(?:env:)?[A-Za-z_][A-Za-z0-9_]*|%[A-Za-z_][A-Za-z0-9_]*%)$/.test(value)
    || /^<[^>]+>$/.test(value)
    || /^(?:(?:your|replace|example|test)[_-][A-Za-z0-9_-]+|change[_-]?me|redacted|dummy|unused|not[_-]needed|lm[_-]?studio|none|x{3,}|\.\.\.)$/i.test(value);
}

function validAddresses(line) {
  return [IPV4, IPV6].flatMap(pattern => [...line.matchAll(pattern)]
    .map(match => match[0]).filter(address => isIP(address)));
}

export function auditMarkdown(path, text) {
  const findings = [];
  for (const [index, line] of text.split(/\r\n|\n|\r/).entries()) {
    if (validAddresses(line).some(address => {
      if (isIP(address) !== 4) return false;
      const [first, second] = address.split('.').map(Number);
      return first === 10 || (first === 172 && second >= 16 && second <= 31)
        || (first === 192 && second === 168);
    })) findings.push(finding(path, index + 1, 'markdown-private-ip'));
    if (PRIVATE_KEY.test(line)) findings.push(finding(path, index + 1, 'markdown-private-key'));
    for (const match of line.matchAll(ASSIGNMENT)) {
      if (CREDENTIAL_NAME.test(match.groups.name.replaceAll('-', '_'))
        && !placeholder(literalValue(line.slice(match.index + match[0].length)))) {
        findings.push(finding(path, index + 1, 'markdown-credential-assignment'));
        break;
      }
    }
  }
  return findings;
}

export function auditEnvExample(text) {
  const findings = [];
  let dockerHostLine = 0;
  for (const [index, line] of text.split(/\r\n|\n|\r/).entries()) {
    const match = line.match(ENV_ASSIGNMENT);
    if (!match) continue;
    const [, name, raw] = match;
    const value = literalValue(raw);
    if (CREDENTIAL_NAME.test(name) && value) {
      findings.push(finding('.env.example', index + 1, 'example-credential-not-blank'));
    }
    if (name === 'GLANCE_DOCKER_HOST') {
      dockerHostLine = index + 1;
      if (value !== 'tcp://docker-proxy:2375') {
        findings.push(finding('.env.example', index + 1, 'example-docker-client-not-proxy'));
      }
    }
  }
  if (!dockerHostLine) findings.push(finding('.env.example', 0, 'example-docker-proxy-setting-missing'));
  return findings;
}

export function auditGlanceYaml(path, text) {
  const findings = [];
  for (const [index, line] of text.split(/\r\n|\n|\r/).entries()) {
    if (URL_LITERAL.test(line)) findings.push(finding(path, index + 1, 'glance-literal-url'));
    if (validAddresses(line).length) findings.push(finding(path, index + 1, 'glance-literal-ip'));
    for (const field of line.matchAll(URL_FIELD)) {
      const value = literalValue(line.slice(field.index + field[0].length));
      if (field.groups.name === 'sock-path') {
        if (!/^\$\{GLANCE_DOCKER_HOST\}(?:\s*[,}].*)?$/.test(value)) {
          findings.push(finding(path, index + 1, 'glance-docker-client-not-env-proxy'));
        }
      } else if (!(ENV_PREFIX.test(value) || /^(?:\/|\.\/|\.\.\/)?assets\//.test(value))) {
        findings.push(finding(path, index + 1, 'glance-endpoint-not-env'));
      }
    }
    for (const match of line.matchAll(HREF)) {
      if (!(ENV_PREFIX.test(match[2]) || /^(?:\/|\.\/|\.\.\/)?assets\//.test(match[2]))) {
        findings.push(finding(path, index + 1, 'glance-href-not-env'));
      }
    }
  }
  return findings;
}

function git(root, ...args) {
  const result = spawnSync('git', ['-C', root, ...args], {
    maxBuffer: 16 * 1024 * 1024, windowsHide: true,
  });
  if (result.error) throw new Error('Cannot run Git');
  return result;
}

function readText(path) {
  if (lstatSync(path).isSymbolicLink()) throw new Error('Cannot audit symbolic links');
  return decoder.decode(readFileSync(path));
}

function* yamlFiles(directory) {
  if (!existsSync(directory)) return;
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error('Cannot audit symbolic links');
    if (entry.isDirectory()) yield* yamlFiles(path);
    else if (entry.isFile() && /\.ya?ml$/i.test(entry.name)) yield path;
  }
}

export function auditRepository(root) {
  const manifest = git(root, 'ls-files', '--cached', '-z');
  const untracked = git(root, 'ls-files', '--others', '--exclude-standard', '-z');
  if (manifest.status !== 0 || untracked.status !== 0) throw new Error('Cannot read Git manifest');
  const tracked = new Set(decoder.decode(manifest.stdout).split('\0').filter(Boolean));
  const files = new Set([...tracked, ...decoder.decode(untracked.stdout).split('\0').filter(Boolean)]);
  const findings = [];
  for (const name of [...files].sort()) {
    const path = join(root, name);
    if (name.split('/').at(-1) === '.env' && tracked.has(name)) {
      findings.push(finding(name, 0, 'real-env-tracked'));
    }
    // Check file names without opening private runtime files, including .env.
    if (!existsSync(path)) continue;
    if (/\.(?:py[wi23]?|ipynb)$/i.test(name)) findings.push(finding(name, 0, 'repository-python-source'));
    if (/\.ps1$/i.test(name)) findings.push(finding(name, 0, 'repository-powershell-script'));
    if (/\.md$/i.test(name)) findings.push(...auditMarkdown(name, readText(path)));
  }
  const ignored = git(root, 'check-ignore', '--quiet', '--no-index', '--', '.env');
  if (ignored.status === 1) findings.push(finding('.gitignore', 0, 'real-env-not-ignored'));
  else if (ignored.status !== 0) throw new Error('Cannot check Git ignore rules');
  const example = join(root, '.env.example');
  if (existsSync(example)) findings.push(...auditEnvExample(readText(example)));
  else findings.push(finding('.env.example', 0, 'example-env-missing'));
  for (const path of yamlFiles(join(root, 'glance', 'config'))) {
    findings.push(...auditGlanceYaml(relative(root, path).split('\\').join('/'), readText(path)));
  }
  const unique = new Map(findings.map(item => [JSON.stringify(item), item]));
  return [...unique.values()].sort((left, right) => {
    if (left.path !== right.path) return left.path < right.path ? -1 : 1;
    return left.line - right.line || (left.rule < right.rule ? -1 : left.rule > right.rule ? 1 : 0);
  });
}

export function main(args = process.argv.slice(2), write = console.log) {
  let root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
  if (args.length) {
    if (args.length !== 2 || args[0] !== '--root' || !args[1]) {
      write('Usage: node audit_repository.mjs [--root DIRECTORY]');
      return 2;
    }
    root = resolve(args[1]);
  }
  let findings;
  try {
    findings = auditRepository(root);
  } catch {
    write('Repository privacy audit could not read the repository.');
    return 2;
  }
  for (const item of findings) write(`${item.path}:${item.line}: ${item.rule}`);
  if (findings.length) {
    write(`Repository privacy audit failed: ${findings.length} finding(s).`);
    return 1;
  }
  write('Repository privacy audit passed.');
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
