import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const source = path.join(root, 'content', 'docs');
const target = path.join(root, 'src', 'content', 'docs');

await rm(target, { recursive: true, force: true });
await mkdir(path.dirname(target), { recursive: true });
await cp(source, target, { recursive: true });

async function normalizeMarkdown(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      await normalizeMarkdown(full);
      continue;
    }

    if (!entry.name.endsWith('.md') && !entry.name.endsWith('.mdx')) continue;

    const sourceText = await readFile(full, 'utf8');
    if (sourceText.startsWith('---\n') || sourceText.startsWith('---\r\n')) continue;

    const h1 = sourceText.match(/^#\s+(.+)$/m)?.[1]?.trim();
    const fallback = path.basename(entry.name, path.extname(entry.name))
      .split('-')
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join(' ');
    const title = (h1 ?? fallback).replaceAll('"', '\\"');

    await writeFile(full, `---\ntitle: "${title}"\n---\n\n${sourceText}`, 'utf8');
  }
}

await normalizeMarkdown(target);

console.log(`Synced public docs: ${source} -> ${target}`);
