import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { versions } from '../versions.mjs';

const dist = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', 'dist');
let checked = 0;

for (const version of versions) {
  const index = await readFile(path.join(dist, 'docs', version.id, 'llms.txt'), 'utf8');
  for (const [, raw] of index.matchAll(/\]\((https:\/\/khoralang\.com\/[^)]+)\)/g)) {
    const url = new URL(raw);
    const file = path.join(dist, decodeURIComponent(url.pathname));
    assert.ok((await stat(file)).isFile(), `${url} has no built file`);
    checked += 1;
  }
}
console.log(`Checked ${checked} absolute index URLs against dist/`);
