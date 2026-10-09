import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { compile, optimize } from '@tailwindcss/node';
import { Scanner } from '@tailwindcss/oxide';

const projectDirectory = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const webDirectory = path.join(projectDirectory, 'web');
const inputPath = path.join(webDirectory, 'input.css');
const outputPath = path.join(webDirectory, 'app.css');
const watchMode = process.argv.slice(2).includes('--watch');

if (process.argv.slice(2).some((argument) => argument !== '--watch')) {
  throw new Error('Usage: node scripts/build-css.mjs [--watch]');
}

function scannerSources(compiler) {
  const sources = [];

  if (compiler.root === null) {
    sources.push({ base: webDirectory, pattern: '**/*', negated: false });
  } else if (compiler.root !== 'none') {
    sources.push({ ...compiler.root, negated: false });
  }

  sources.push(...compiler.sources);
  sources.push({
    base: webDirectory,
    pattern: path.basename(inputPath),
    negated: false,
  });

  return sources;
}

function watchPathsFor(compiler, dependencies) {
  if (compiler.root !== 'none') {
    throw new Error('Watch mode requires source(none) and explicit @source file paths.');
  }
  const watchPaths = new Set([inputPath, ...dependencies]);
  for (const source of compiler.sources) {
    if (source.negated || /[*?{}\[\]]/.test(source.pattern)) {
      throw new Error('Watch mode currently requires explicit @source file paths.');
    }
    watchPaths.add(path.resolve(source.base, source.pattern));
  }
  return watchPaths;
}

async function buildCss({ minify, watch = false }) {
  const input = await readFile(inputPath, 'utf8');
  const dependencies = new Set();
  const compiler = await compile(input, {
    from: inputPath,
    base: webDirectory,
    onDependency(dependency) {
      dependencies.add(path.resolve(dependency));
    },
  });
  const watchPaths = watch ? watchPathsFor(compiler, dependencies) : undefined;
  const scanner = new Scanner({ sources: scannerSources(compiler) });
  const css = compiler.build(scanner.scan());
  const rendered = (minify ? optimize(css, { minify: true }).code : css)
    .replace(/\r\n?/g, '\n');

  let current = '';
  try {
    current = await readFile(outputPath, 'utf8');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }

  if (current !== rendered) {
    await writeFile(outputPath, rendered);
  }

  return { changed: current !== rendered, watchPaths };
}

if (!watchMode) {
  await buildCss({ minify: true });
} else {
  let watchPaths;
  const fingerprint = async (paths) => {
    const hash = createHash('sha256');
    for (const sourcePath of [...paths].sort()) {
      hash.update(sourcePath);
      try {
        hash.update(await readFile(sourcePath));
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
        hash.update('<missing>');
      }
    }
    return hash.digest('hex');
  };

  const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
  let stopping = false;
  process.once('SIGINT', () => { stopping = true; });
  process.once('SIGTERM', () => { stopping = true; });

  let result = await buildCss({ minify: false, watch: true });
  watchPaths = result.watchPaths;
  let previous = await fingerprint(watchPaths);
  console.log('Watching Tailwind CSS input and source files. Press Ctrl+C to stop.');

  while (!stopping) {
    await delay(350);
    let current = await fingerprint(watchPaths);
    if (current === previous) continue;

    await delay(120);
    current = await fingerprint(watchPaths);
    if (current === previous) continue;
    result = await buildCss({ minify: false, watch: true });
    watchPaths = result.watchPaths;
    previous = await fingerprint(watchPaths);
    console.log('Rebuilt web/app.css');
  }
}
