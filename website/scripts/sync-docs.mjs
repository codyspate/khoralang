import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const source = path.join(root, 'content', 'docs');
const collectionRoot = path.join(root, 'src', 'content', 'docs');
const target = path.join(collectionRoot, 'docs');

function lineAt(text, index) {
  return text.slice(0, index).split(/\r?\n/).length;
}

function isDocFile(name) {
  return name.endsWith('.md') || name.endsWith('.mdx');
}

async function collectDocFiles(dir) {
  const files = [];

  async function visit(current) {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) {
        await visit(full);
      } else if (isDocFile(entry.name)) {
        files.push(full);
      }
    }
  }

  await visit(dir);
  return files;
}

function routeForFile(file) {
  const relative = path.relative(source, file).split(path.sep).join('/');
  const withoutExtension = relative.replace(/\.(?:md|mdx)$/i, '');
  const routePath = withoutExtension.endsWith('/index')
    ? withoutExtension.slice(0, -'/index'.length)
    : withoutExtension === 'index'
      ? ''
      : withoutExtension;
  return `/docs/${routePath ? `${routePath}/` : ''}`;
}

function linksIn(text) {
  const found = [];
  const patterns = [
    // Markdown links, but not images. Link titles after the URL are allowed.
    /(?<!!)\[[^\]]*\]\(\s*<?([^\s)>]+)>?[^)]*\)/g,
    // Raw HTML links.
    /href\s*=\s*["']([^"']+)["']/gi,
    // Reference-style Markdown: [label]: /docs/page/
    /^\s*\[[^\]]+\]:\s*<?([^\s>]+)>?/gm,
  ];

  for (const pattern of patterns) {
    for (const match of text.matchAll(pattern)) {
      found.push({ url: match[1], line: lineAt(text, match.index ?? 0) });
    }
  }

  return found;
}

function internalRoute(url, fromRoute) {
  const raw = url.trim().replace(/^<|>$/g, '');
  if (!raw || raw.startsWith('#')) return fromRoute;
  if (/^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith('//')) return null;

  const resolved = new URL(raw, `https://khoralang.com${fromRoute}`);
  if (!resolved.pathname.startsWith('/docs/')) return null;

  let pathname = resolved.pathname.replace(/\/+/g, '/');
  if (!pathname.endsWith('/')) pathname += '/';
  return pathname;
}

async function validateDocLinks(dir) {
  const files = await collectDocFiles(dir);
  const routeByFile = new Map(files.map((file) => [file, routeForFile(file)]));
  const knownRoutes = new Set(routeByFile.values());
  const broken = [];

  for (const file of files) {
    const text = await readFile(file, 'utf8');
    const fromRoute = routeByFile.get(file);

    for (const link of linksIn(text)) {
      const renderedRoute = internalRoute(link.url, fromRoute);
      if (!renderedRoute) continue;

      const sourceUrl = /\.mdx?(?:[?#].*)?$/i.test(link.url.trim().replace(/^<|>$/g, ''));
      if (sourceUrl || !knownRoutes.has(renderedRoute)) {
        const reason = sourceUrl
          ? 'source filename is not a rendered route'
          : `resolves to missing route ${renderedRoute}`;
        broken.push(
          `${path.relative(source, file)}:${link.line} -> ${link.url} (${reason})`,
        );
      }
    }
  }

  if (broken.length > 0) {
    throw new Error(
      `Broken internal documentation links:\n${broken.map((link) => `  ${link}`).join('\n')}`,
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

    if (!isDocFile(entry.name)) continue;

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
