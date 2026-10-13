import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { current, versions } from '../versions.mjs';

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const publicDir = path.join(site, 'public');

async function pagesIn(directory) {
  const pages = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) pages.push(...await pagesIn(full));
    else if (/\.mdx?$/.test(entry.name)) pages.push(full);
  }
  return pages;
}

function route(file, source, version) {
  const relative = path.relative(source, file).replace(/\\/g, '/').replace(/\.mdx?$/, '');
  const part = relative === 'index' ? '' : relative.replace(/\/index$/, '');
  return `https://khoralang.com/docs/${version}/${part ? `${part}/` : ''}index.md`;
}

async function generated(version, name) {
  return readFile(path.join(publicDir, 'docs', version, name), 'utf8');
}

test('the stable and next indexes cover their source pages in sidebar section order', async () => {
  for (const version of versions) {
    const source = path.join(site, version.from);
    const files = await pagesIn(source);
    const index = await generated(version.id, 'llms.txt');
    const full = await generated(version.id, 'llms-full.txt');
    assert.match(index, /^# Khora\n\n> /);
    assert.match(index, /## Optional\n/);
    assert.ok(index.includes(`https://khoralang.com/docs/${version.id}/llms-full.txt`));
    assert.ok(index.includes(`https://khoralang.com/docs/${version.id}/llms-full-guide.txt`));
    const guide = await generated(version.id, 'llms-full-guide.txt');
    assert.doesNotMatch(guide, /Source: .*\/stdlib\/api\//);
    assert.match(full, /Source: .*\/stdlib\/api\//);
    assert.ok(index.indexOf('## Getting Started') < index.indexOf('## Language Reference'));
    assert.ok(index.indexOf('## Language Reference') < index.indexOf('## Standard Library'));
    if (index.includes('## Packages')) {
      assert.ok(index.indexOf('## Standard Library') < index.indexOf('## Packages'));
    }
    for (const file of files) {
      const url = route(file, source, version.id);
      assert.ok(index.includes(`](${url})`), `${version.id}: index missing ${url}`);
      assert.ok(full.includes(`Source: ${url}\n`), `${version.id}: full missing ${url}`);
    }
    assert.equal((index.match(/^Source: /gm) ?? []).length, 0);
    assert.equal((full.match(/^Source: /gm) ?? []).length, files.length);
    assert.doesNotMatch(full, /(?:^|\n)---\r?\ntitle:/);
    assert.doesNotMatch(full, /^import\s+\S+\s+from\s+['"]/m);
  }
  assert.equal(await readFile(path.join(publicDir, 'llms.txt'), 'utf8'), await generated(current, 'llms.txt'));
  assert.equal(await readFile(path.join(publicDir, 'llms-full.txt'), 'utf8'), await generated(current, 'llms-full.txt'));
  assert.match(await generated('next', 'llms.txt'), /unreleased/);
  assert.match(await generated(current, 'llms.txt'), /current stable/);
  assert.match(await readFile(path.join(publicDir, 'llms-full.txt'), 'utf8'), /```khora\n/);
});

test('versioned Markdown keeps internal links within its own tree', async () => {
  const markdown = await readFile(path.join(publicDir, 'docs', 'v0.4', 'index.md'), 'utf8');
  assert.match(markdown, /\]\(\/docs\/v0\.4\/reference\/compatibility\/\)/);
});

test('discoverability is visible on both the current release and next', async () => {
  for (const version of new Set([current, 'next'])) {
    const source = path.join(site, versions.find((entry) => entry.id === version).from);
    for (const page of ['index.md', 'getting-started/installation.md']) {
      const text = await readFile(path.join(source, page), 'utf8');
      assert.match(text, /Reading this as an AI agent\?.*\/llms\.txt.*\/llms-full\.txt/);
    }
    const editor = await readFile(path.join(source, 'getting-started/editor.md'), 'utf8');
    assert.match(editor, /khora std search <query>[\s\S]*\/llms\.txt/);
    assert.match(editor, /khora mcp/);
    const html = await readFile(path.join(site, 'dist', 'docs', version, 'index.html'), 'utf8');
    assert.match(html, /rel="alternate" type="text\/plain" href="\/llms\.txt" title="LLM-readable docs"/);
  }
});

test('inline code with type arguments and placeholders survives the all-in-one file', async () => {
  const full = await readFile(path.join(publicDir, 'llms-full.txt'), 'utf8');
  for (const span of ['`Option<A>`', '`khora std search <query>`']) {
    assert.ok(full.includes(span), `llms-full.txt lost ${span}`);
  }
  // Maintainer comments are notes, not documentation, even across lines.
  assert.doesNotMatch(full, /<!--/);
});

test('each indexed URL is a Markdown file served from public', async () => {
  const index = await readFile(path.join(publicDir, 'llms.txt'), 'utf8');
  for (const match of index.matchAll(/\]\((https:\/\/khoralang\.com\/[^)]+)\)/g)) {
    const url = new URL(match[1]);
    const target = path.join(publicDir, decodeURIComponent(url.pathname));
    assert.ok((await stat(target)).isFile(), `no file for ${url}`);
  }
});
