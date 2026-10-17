// Development-only minification. Checked-in output keeps production images free of Node.js.
import { transform } from 'esbuild';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const check = process.argv.includes('--check');
const directory = new URL('./dist/', import.meta.url);
if (!check) await mkdir(directory, { recursive: true });
for (const name of ['app.js', 'live.js', 'style.css']) {
  const source = await readFile(new URL(name, import.meta.url), 'utf8');
  const { code } = await transform(source, {
    loader: name.endsWith('.css') ? 'css' : 'js', minify: true,
    target: ['chrome110', 'firefox115', 'safari16'], charset: 'utf8', legalComments: 'none',
  });
  const output = new URL(name.replace(/\.(js|css)$/, '.min.$1'), directory);
  if (check) {
    if (await readFile(output, 'utf8') !== code) throw new Error(`Regenerate ${fileURLToPath(output)} with npm run build`);
  } else await writeFile(output, code);
}
