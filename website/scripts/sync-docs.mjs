import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const source = path.join(root, 'content', 'docs');
const collectionRoot = path.join(root, 'src', 'content', 'docs');
const target = path.join(collectionRoot, 'docs');

function isInternalMarkdownSourceUrl(url) {
  const targetUrl = url.trim().replace(/^<|>$/g, '');
  if (/^[a-z][a-z0-9+.-]*:/i.test(targetUrl) || targetUrl.startsWith('//')) return false;
  return /\.md(?:[?#].*)?$/i.test(targetUrl);
}

function lineAt(text, index) {
  return text.slice(0, index).split(/\r?\n/).length;
}

function sourceFileLinks(text) {
  const found = [];
  const patterns = [
    // Inline Markdown links and images. The public site routes to pages, not
    // the .md source files Starlight consumed.
    /!?\[[^\]]*\]\(\s*<?([^\s)>]+)>?[^)]*\)/g,
    // Raw HTML occasionally appears in docs and follows the same URL rule.
    /href\s*=\s*["']([^"']+)["']/gi,
    // Reference-style Markdown: [label]: ./page.md
    /^\s*\[[^\]]+\]:\s*<?([^\s>]+)>?/gm,
  ];

  for (const pattern of patterns) {
    for (const match of text.matchAll(pattern)) {
      const url = match[1];
      if (isInternalMarkdownSourceUrl(url)) {
        found.push({ url, line: lineAt(text, match.index ?? 0) });
      }
    }
  }

  return found;
}

async function validateDocLinks(dir) {
  const broken = [];

  async function visit(current) {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) {
        await visit(full);
        continue;
      }
      if (!entry.name.endsWith('.md') && !entry.name.endsWith('.mdx')) continue;

      const text = await readFile(full, 'utf8');
      for (const link of sourceFileLinks(text)) {
        broken.push(`${path.relative(source, full)}:${link.line} -> ${link.url}`);
      }
    }
  }

  await visit(dir);
  if (broken.length > 0) {
    throw new Error(
      `Public documentation links must use rendered routes, not .md source URLs:\n${broken
        .map((link) => `  ${link}`)
        .join('\n')}`,
    );
  }
}

await validateDocLinks(source);
await rm(collectionRoot, { recursive: true, force: true });
await mkdir(target, { recursive: true });
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

const apiSections = new Set([
  'Types',
  'Traits',
  'Effects',
  'Contexts',
  'Methods',
  'Trait implementations',
  'Functions',
  'Constants',
]);

function anchorFor(heading) {
  return heading
    .trim()
    .toLowerCase()
    .replace(/[`*_~]/g, '')
    .replace(/[^\p{L}\p{N}\s-]/gu, '')
    .replace(/\s+/g, '-')
    .replace(/-+/g, '-');
}

function apiIndex(text) {
  const groups = [];
  let current = null;

  for (const line of text.split(/\r?\n/)) {
    const section = line.match(/^## ([^#].*)$/)?.[1]?.trim();
    if (section !== undefined) {
      current = apiSections.has(section) ? { name: section, items: [] } : null;
      if (current) groups.push(current);
      continue;
    }

    if (!current) continue;
    const item = line.match(/^### ([^#].*)$/)?.[1]?.trim();
    if (item) current.items.push(item);
  }

  const populated = groups.filter((group) => group.items.length > 0);
  if (populated.length === 0) return '';

  const lines = ['## API at a glance', ''];
  for (const group of populated) {
    const links = group.items
      .map((item) => `[\`${item}\`](#${anchorFor(item)})`)
      .join(' · ');
    lines.push(`**${group.name}:** ${links}`, '');
  }
  return `${lines.join('\n')}\n`;
}

function compactFieldSignatures(text) {
  return text.replace(
    /^#### ([^\n]+)\n\n```khora\n([^\n]+)\n```\n/gm,
    (whole, name, signature) => {
      const bare = signature.startsWith('mut ') ? signature.slice(4) : signature;
      if (!bare.startsWith(`${name}:`)) return whole;
      return `#### ${name}\n\n<code class="khora-member-signature">${signature}</code>\n`;
    },
  );
}

async function enhanceGeneratedApi(dir) {
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch (error) {
    if (error?.code === 'ENOENT') return;
    throw error;
  }

  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      await enhanceGeneratedApi(full);
      continue;
    }
    if (!entry.name.endsWith('.md')) continue;

    let text = await readFile(full, 'utf8');
    if (!text.includes('<!-- Generated by `khora doc`')) continue;

    text = compactFieldSignatures(text);

    const index = apiIndex(text);
    if (index) {
      const firstApiSection = text.search(
        /^## (Types|Traits|Effects|Contexts|Methods|Trait implementations|Functions|Constants)\s*$/m,
      );
      if (firstApiSection >= 0) {
        text = `${text.slice(0, firstApiSection)}${index}${text.slice(firstApiSection)}`;
      }
    }

    const frontmatterEnd = text.indexOf('\n---\n', 4);
    if (frontmatterEnd >= 0) {
      const insertAt = frontmatterEnd + '\n---\n'.length;
      text = `${text.slice(0, insertAt)}\n<span class="khora-api-reference-marker" aria-hidden="true"></span>\n${text.slice(insertAt)}`;
    }

    await writeFile(full, text, 'utf8');
  }
}

await normalizeMarkdown(target);
await enhanceGeneratedApi(path.join(target, 'stdlib', 'api'));

console.log(`Synced public docs: ${source} -> ${target}`);
