import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const source = path.join(root, 'content', 'docs');
const collectionRoot = path.join(root, 'src', 'content', 'docs');
const target = path.join(collectionRoot, 'docs');
const unstableBanner =
  'Khora is unstable before v1. Syntax, standard-library APIs, and behavior may change before v1.';

// --- what this build was made from ------------------------------------------
//
// A deployed page that cannot say which commit produced it is a page nobody
// can check. Somebody reading `/docs/reference/traps/` and finding it disagrees
// with their compiler has two candidate explanations and no way to tell them
// apart; a revision in the footer settles it in one click.

/// The commit this build came from, or `null` if nothing can say.
///
/// **CI first.** A checkout in a build container may be shallow or detached,
/// so `git rev-parse HEAD` there is not reliably the commit being deployed.
/// `GITHUB_SHA` is what the workflow was triggered on.
///
/// `null` rather than `"unknown"`: a footer that says it was built from
/// nothing in particular has spent a line saying nothing. The component leaves
/// the revision out instead.
function revisionOf() {
  const fromCi = process.env.GITHUB_SHA || process.env.CF_PAGES_COMMIT_SHA;
  if (fromCi) return fromCi;
  try {
    return execFileSync('git', ['rev-parse', 'HEAD'], {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return null;
  }
}

/// The language release these pages describe.
///
/// From `khora.toml`, not from `package.json`: the site's own version is about
/// the site, and what a reader wants is which *language* release they are
/// reading about.
async function releaseOf() {
  try {
    const manifest = await readFile(path.join(root, '..', 'khora.toml'), 'utf8');
    const found = manifest.match(/^\s*version\s*=\s*"([^"]+)"/m);
    return found ? found[1] : null;
  } catch {
    return null;
  }
}

/// Writes what this build was made from, for the footer to read.
///
/// A module rather than JSON in `public/`, so that a page importing it fails
/// the build when it is missing rather than rendering an empty footer.
async function writeProvenance() {
  const revision = revisionOf();
  const release = await releaseOf();
  const built = {
    revision,
    short: revision ? revision.slice(0, 12) : null,
    release,
    // To the minute. A second is precision this does not have -- two builds of
    // one commit are the same site -- and a date alone would not tell two
    // builds of one day apart.
    builtAt: new Date().toISOString().slice(0, 16).replace('T', ' ') + 'Z',
  };
  const file = path.join(root, 'src', 'generated', 'build.js');
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(
    file,
    '// Written by scripts/sync-docs.mjs. Not edited by hand, not committed.\n'
      + `export const build = ${JSON.stringify(built, null, 2)};\n`,
    'utf8',
  );
  return built;
}

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

function isExternalUrl(raw) {
  return /^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith('//');
}

function splitUrl(raw) {
  const hashAt = raw.indexOf('#');
  const queryAt = raw.indexOf('?');
  const cutAt = [hashAt, queryAt].filter((index) => index >= 0).sort((a, b) => a - b)[0];
  return cutAt === undefined
    ? { pathname: raw, suffix: '' }
    : { pathname: raw.slice(0, cutAt), suffix: raw.slice(cutAt) };
}

function looksLikeDocPath(pathname) {
  if (!pathname) return true;
  if (pathname.endsWith('/')) return true;
  return !path.posix.basename(pathname).includes('.');
}

function sourceRelativeRoute(url, fromFile) {
  const raw = url.trim().replace(/^<|>$/g, '');
  if (!raw || raw.startsWith('#') || isExternalUrl(raw)) return null;
  if (raw.startsWith('/')) return null;

  const { pathname, suffix } = splitUrl(raw);
  if (!looksLikeDocPath(pathname)) return null;

  const relativeSourceFile = path.relative(source, fromFile).split(path.sep).join('/');
  const base = new URL(relativeSourceFile, 'https://khora-doc-source.invalid/');
  const resolved = new URL(pathname || '.', base);
  let routePath = resolved.pathname.replace(/^\/+/, '').replace(/\/+/g, '/');
  if (!routePath.endsWith('/')) routePath += '/';
  return { route: `/docs/${routePath}`, suffix };
}

function absoluteDocRoute(url) {
  const raw = url.trim().replace(/^<|>$/g, '');
  if (!raw.startsWith('/docs/')) return null;
  const { pathname, suffix } = splitUrl(raw);
  let route = pathname.replace(/\/+/g, '/');
  if (!route.endsWith('/')) route += '/';
  return { route, suffix };
}

function linksIn(text) {
  const found = [];
  const patterns = [
    /(?<!!)\[[^\]]*\]\(\s*<?([^\s)>]+)>?[^)]*\)/g,
    /href\s*=\s*["']([^"']+)["']/gi,
    /^\s*\[[^\]]+\]:\s*<?([^\s>]+)>?/gm,
  ];

  for (const pattern of patterns) {
    for (const match of text.matchAll(pattern)) {
      found.push({ url: match[1], line: lineAt(text, match.index ?? 0) });
    }
  }

  return found;
}

function resolvedDocLink(url, fromFile) {
  const raw = url.trim().replace(/^<|>$/g, '');
  // **External first.** The `.md` test below is about somebody linking to a
  // source file in this tree instead of to the route it renders as -- a real
  // mistake, and one worth failing the build over. It is not about
  // `https://github.com/.../CONTRIBUTING.md`, which is a link to a file that
  // is meant to be read as a file and is the correct thing to write.
  //
  // The order was the other way round, so three links added in 13.14 and 13.15
  // broke `npm run build` and nothing noticed: the repository's own gate does
  // not build the site, and CI only runs on a push.
  if (isExternalUrl(raw) || raw.startsWith('//')) return null;
  if (/\.mdx?(?:[?#].*)?$/i.test(raw)) {
    return { error: 'source filename is not a rendered route' };
  }
  return absoluteDocRoute(raw) ?? sourceRelativeRoute(raw, fromFile);
}

async function validateDocLinks(files, knownRoutes) {
  const broken = [];

  for (const file of files) {
    const text = await readFile(file, 'utf8');
    for (const link of linksIn(text)) {
      const resolved = resolvedDocLink(link.url, file);
      if (!resolved) continue;
      if (resolved.error) {
        broken.push(`${path.relative(source, file)}:${link.line} -> ${link.url} (${resolved.error})`);
        continue;
      }
      if (!knownRoutes.has(resolved.route)) {
        broken.push(
          `${path.relative(source, file)}:${link.line} -> ${link.url} (resolves to missing route ${resolved.route})`,
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

function rewriteDocUrl(url, fromFile, knownRoutes) {
  const resolved = resolvedDocLink(url, fromFile);
  if (!resolved || resolved.error || !knownRoutes.has(resolved.route)) return url;
  return `${resolved.route}${resolved.suffix}`;
}

function rewriteDocLinks(text, fromFile, knownRoutes) {
  text = text.replace(
    /((?<!!)\[[^\]]*\]\(\s*<?)([^\s)>]+)(>?[^)]*\))/g,
    (whole, before, url, after) => `${before}${rewriteDocUrl(url, fromFile, knownRoutes)}${after}`,
  );
  text = text.replace(
    /(href\s*=\s*["'])([^"']+)(["'])/gi,
    (whole, before, url, after) => `${before}${rewriteDocUrl(url, fromFile, knownRoutes)}${after}`,
  );
  return text.replace(
    /^(\s*\[[^\]]+\]:\s*<?)([^\s>]+)(>?)/gm,
    (whole, before, url, after) => `${before}${rewriteDocUrl(url, fromFile, knownRoutes)}${after}`,
  );
}

function addBanner(text) {
  const bannerYaml = `banner:\n  content: "${unstableBanner}"\n`;
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) return text;
  const frontmatterEnd = text.indexOf('\n---\n', 4);
  if (frontmatterEnd < 0) return text;
  const frontmatter = text.slice(0, frontmatterEnd);
  if (/^banner\s*:/m.test(frontmatter)) return text;
  return `${text.slice(0, frontmatterEnd)}\n${bannerYaml}${text.slice(frontmatterEnd)}`;
}

const sourceFiles = await collectDocFiles(source);
const knownRoutes = new Set(sourceFiles.map(routeForFile));
await validateDocLinks(sourceFiles, knownRoutes);

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

    const relative = path.relative(target, full);
    const canonicalSource = path.join(source, relative);
    let text = await readFile(full, 'utf8');

    if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) {
      const h1 = text.match(/^#\s+(.+)$/m)?.[1]?.trim();
      const fallback = path.basename(entry.name, path.extname(entry.name))
        .split('-')
        .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
        .join(' ');
      const title = (h1 ?? fallback).replaceAll('"', '\\"');
      text = `---\ntitle: "${title}"\n---\n\n${text}`;
    }

    text = rewriteDocLinks(text, canonicalSource, knownRoutes);
    text = addBanner(text);
    await writeFile(full, text, 'utf8');
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

const built = await writeProvenance();
console.log(`Synced public docs: ${source} -> ${target}`);
console.log(
  built.short
    ? `Built from ${built.short}${built.release ? ` (${built.release})` : ''} at ${built.builtAt}`
    : 'No revision available; the footer will not claim one',
);
