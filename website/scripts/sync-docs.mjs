import { cp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

import { current, editUrlFor, stable, versions } from '../versions.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const collectionRoot = path.join(root, 'src', 'content', 'docs');

/// The banner a reader of `version` should see, or `null` for none.
///
/// **Three cases, and the third is the one versioning exists for.** The
/// unreleased tree says so. A stable tree that is no longer current says so
/// loudly, because somebody who arrived from a search result has no other way
/// to find out. The current stable tree says nothing at all — a banner on the
/// page everybody is supposed to be reading is a banner everybody learns to
/// ignore, and then the one that matters is invisible too.
function bannerFor(version) {
  if (!version.stable) {
    const pointer = stable
      ? ` The current release is documented under <a href="/docs/${stable.id}/">${stable.label}</a>.`
      : '';
    return `You are reading <strong>next</strong>, which describes the unreleased `
      + `compiler. Khora is unstable before v1: syntax, standard-library APIs and `
      + `behavior may change.${pointer}`;
  }
  if (version.id !== current) {
    return `You are reading <strong>${version.label}</strong>, which is not the `
      + `current release. <a href="/docs/${current}/">Go to ${current}</a>.`;
  }
  return null;
}

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
///
/// **This is `next`'s answer only.** A released tree describes the compiler its
/// tag published, which is not the one in this checkout — see `releaseFor`.
async function releaseOf() {
  try {
    const manifest = await readFile(path.join(root, '..', 'khora.toml'), 'utf8');
    const found = manifest.match(/^\s*version\s*=\s*"([^"]+)"/m);
    return found ? found[1] : null;
  } catch {
    return null;
  }
}

/// The release each documentation tree describes, keyed by its id.
///
/// **A footer that names one version for the whole site is wrong on every page
/// but one.** `/docs/v0.1/` pages carried "Khora 0.2.0" because the version came
/// from `khora.toml` in this checkout — so a reader on the tree that documents
/// the compiler they can install was told they were reading about a compiler
/// that is not released. Three separate readers reported it as the reason they
/// could not tell which tree matched their toolchain.
///
/// A stable tree's version is its `cutFrom` tag with the leading `v` dropped:
/// the tag is what published the compiler the tree describes, and
/// `check-released-docs.sh` already holds that claim honest. `next` is the
/// working tree and takes `khora.toml`.
function releasesByTree(nextRelease) {
  return Object.fromEntries(
    versions.map((each) => [
      each.id,
      each.cutFrom ? each.cutFrom.replace(/^v/, '') : nextRelease,
    ]),
  );
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
    /// The release each tree describes, so the footer can name the right one
    /// for the page it is on rather than one for the whole site.
    releases: releasesByTree(release),
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

function routeForFile(file, source, version) {
  const relative = path.relative(source, file).split(path.sep).join('/');
  const withoutExtension = relative.replace(/\.(?:md|mdx)$/i, '');
  const routePath = withoutExtension.endsWith('/index')
    ? withoutExtension.slice(0, -'/index'.length)
    : withoutExtension === 'index'
      ? ''
      : withoutExtension;
  return `/docs/${version}/${routePath ? `${routePath}/` : ''}`;
}

// **A markdown link label can contain brackets, and the one that broke did.**
//
// The label was `` [`[toolchain]`] `` -- a code span naming a TOML table --
// and the pattern here used to be `\[[^\]]*\]`, which stops at the first `]`
// and so cannot match it. That was not merely a missed rewrite: `linksIn` and
// `rewriteDocLinks` share this pattern, so the *same* blindness meant
// `validateDocLinks` never looked at the link either. The build's own link
// checker was silent about exactly the class of link the build failed to
// rewrite, and a `/docs/reference/manifest/` with no page behind it shipped.
//
// So the label allows one level of nesting: either a character that is not a
// bracket, or a whole bracketed run. One level is what CommonMark itself
// permits inside a link label, and it is what a code span, an emphasis marker
// or a nested `[...]` needs. The alternation cannot cross an unmatched `]`,
// so `[a] [b](c)` still matches only `[b](c)`.
const LINK_LABEL = String.raw`\[(?:[^\[\]]|\[[^\[\]]*\])*\]`;

/// `[label](url "title")`, with the label allowed to contain brackets.
///
/// `split: true` returns three groups -- everything before the URL, the URL,
/// and everything after -- so a replacement can put a rewritten URL back
/// without reconstructing the label. Without it there is one group, the URL.
function inlineLinkPattern({ split = false } = {}) {
  const url = String.raw`[^\s)>]+`;
  return split
    ? new RegExp(String.raw`((?<!!)${LINK_LABEL}\(\s*<?)(${url})(>?[^)]*\))`, 'g')
    : new RegExp(String.raw`(?<!!)${LINK_LABEL}\(\s*<?(${url})>?[^)]*\)`, 'g');
}

/// `[label]: url`, the reference-style definition, with the same label rule.
function referenceDefinitionPattern({ split = false } = {}) {
  const url = String.raw`[^\s>]+`;
  return split
    ? new RegExp(String.raw`^(\s*${LINK_LABEL}:\s*<?)(${url})(>?)`, 'gm')
    : new RegExp(String.raw`^\s*${LINK_LABEL}:\s*<?(${url})>?`, 'gm');
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

function sourceRelativeRoute(url, fromFile, source, version) {
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
  return { route: `/docs/${version}/${routePath}`, suffix };
}

/// A `/docs/...` link written in a source page, as a route in `version`.
///
/// **The source tree is version-agnostic and its links are too.** A page writes
/// `/docs/reference/traps/` because that is what it means; which tree it lands
/// in is decided here, when the page is copied into one. A link that already
/// names a version is left alone, so a `next` page can point at a released one
/// on purpose.
function absoluteDocRoute(url, version) {
  const raw = url.trim().replace(/^<|>$/g, '');
  if (!raw.startsWith('/docs/')) return null;
  const { pathname, suffix } = splitUrl(raw);
  let route = pathname.replace(/\/+/g, '/');
  if (!route.endsWith('/')) route += '/';
  const segment = route.slice('/docs/'.length).split('/')[0];
  if (versions.some((each) => each.id === segment)) return { route, suffix };
  return { route: `/docs/${version}/${route.slice('/docs/'.length)}`, suffix };
}

function linksIn(text) {
  const found = [];
  const patterns = [
    inlineLinkPattern(),
    /href\s*=\s*["']([^"']+)["']/gi,
    referenceDefinitionPattern(),
  ];

  for (const pattern of patterns) {
    for (const match of text.matchAll(pattern)) {
      found.push({ url: match[1], line: lineAt(text, match.index ?? 0) });
    }
  }

  return found;
}

function resolvedDocLink(url, fromFile, source, version) {
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
  return absoluteDocRoute(raw, version) ?? sourceRelativeRoute(raw, fromFile, source, version);
}

/// Every heading anchor a page offers, as the slugs a link may name.
///
/// Headings inside a fenced code block are not headings -- `# comment` in a
/// shell example is the common one -- so the fences are tracked.
/// Every anchor a page offers, numbered the way the renderer numbers them.
///
/// **Repeated headings get a suffix, because Starlight gives them one.** It
/// takes each heading's id from `github-slugger`, which answers `get`,
/// `get-1`, `get-2`, `get-3` for four headings called `get` -- and
/// `stdlib/api/core.md` has exactly four, because it documents a method of
/// that name on four types.
///
/// This collapsed them all to `get`, so a link to the fourth was reported as
/// pointing at a heading that does not exist while the built page had that id.
/// Three implementations of one rule -- `khora doc`'s, this one, and the
/// renderer's -- and this was the one that disagreed.
function anchorsIn(text) {
  const anchors = new Set();
  const seen = new Map();
  let fenced = false;
  for (const line of text.split(/\r?\n/)) {
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) continue;
    const heading = line.match(/^#{1,6}\s+(.+?)\s*#*\s*$/)?.[1];
    if (heading) {
      const base = anchorFor(heading);
      const before = seen.get(base) ?? 0;
      seen.set(base, before + 1);
      anchors.add(before === 0 ? base : base + '-' + before);
    }
    for (const explicit of line.matchAll(/\bid\s*=\s*["']([^"']+)["']/g)) {
      anchors.add(explicit[1]);
    }
  }
  return anchors;
}

async function validateDocLinks(files, knownRoutes, source, version, anchorsAreFatal) {
  const broken = [];
  // **A link to a heading that does not exist is checked too**, because it is
  // the same failure as a link to a page that does not exist and it is the one
  // a reader hits after clicking: the page loads, and the browser sits at the
  // top of it.
  //
  // **Fatal for `next`, reported for a released tree**, which `versions.mjs`
  // already draws the line for: `next` is the tree this repository gates, and a
  // tree under `content/versions/` describes a compiler this checkout is not,
  // is not maintained against it, and must not be edited to make a gate on
  // `next` go green. So an old tree's dangling anchors are printed, with file
  // and line, on every build -- visible rather than enforced, which is the
  // opposite of a checker that says nothing at all.
  const danglingAnchors = [];
  const anchorsByRoute = new Map();

  for (const file of files) {
    anchorsByRoute.set(routeForFile(file, source, version), anchorsIn(await readFile(file, 'utf8')));
  }

  for (const file of files) {
    const text = await readFile(file, 'utf8');
    for (const link of linksIn(text)) {
      const resolved = resolvedDocLink(link.url, file, source, version);
      const where = `${path.relative(source, file)}:${link.line}`;
      if (!resolved) {
        // A bare `#anchor` names a heading on this page, and is the one link
        // that never resolves to a route.
        const own = link.url.trim();
        if (own.startsWith('#') && own.length > 1) {
          const anchors = anchorsByRoute.get(routeForFile(file, source, version));
          if (anchors && !anchors.has(own.slice(1))) {
            danglingAnchors.push(`${where} -> ${own} (no heading on this page)`);
          }
        }
        continue;
      }
      if (resolved.error) {
        broken.push(`${where} -> ${link.url} (${resolved.error})`);
        continue;
      }
      if (!knownRoutes.has(resolved.route)) {
        broken.push(`${where} -> ${link.url} (resolves to missing route ${resolved.route})`);
        continue;
      }
      const fragment = resolved.suffix.startsWith('#') ? resolved.suffix.slice(1) : '';
      if (fragment) {
        const anchors = anchorsByRoute.get(resolved.route);
        if (anchors && !anchors.has(fragment)) {
          danglingAnchors.push(`${where} -> ${link.url} (${resolved.route} has no #${fragment})`);
        }
      }
    }
  }

  if (danglingAnchors.length > 0) {
    const report = `${version}: ${danglingAnchors.length} link(s) to a heading that does not exist:\n`
      + danglingAnchors.map((link) => `  ${link}`).join('\n');
    if (anchorsAreFatal) broken.push(...danglingAnchors);
    else console.warn(report);
  }

  if (broken.length > 0) {
    throw new Error(
      `Broken internal documentation links:\n${broken.map((link) => `  ${link}`).join('\n')}`,
    );
  }
}

function rewriteDocUrl(url, fromFile, knownRoutes, source, version) {
  const resolved = resolvedDocLink(url, fromFile, source, version);
  if (!resolved || resolved.error || !knownRoutes.has(resolved.route)) return url;
  return `${resolved.route}${resolved.suffix}`;
}

function rewriteDocLinks(text, fromFile, knownRoutes, source, version) {
  const rewrite = (url) => rewriteDocUrl(url, fromFile, knownRoutes, source, version);
  text = text.replace(
    inlineLinkPattern({ split: true }),
    (whole, before, url, after) => `${before}${rewrite(url)}${after}`,
  );
  text = text.replace(
    /(href\s*=\s*["'])([^"']+)(["'])/gi,
    (whole, before, url, after) => `${before}${rewrite(url)}${after}`,
  );
  return text.replace(
    referenceDefinitionPattern({ split: true }),
    (whole, before, url, after) => `${before}${rewrite(url)}${after}`,
  );
}

/// Point "Edit this page" at the file somebody can actually edit.
///
/// Written per page rather than left to `editLink.baseUrl` in the config,
/// because the two trees have two different source roots and Starlight has one
/// base URL. `versions.mjs`'s `editUrlFor` has the argument in full.
///
/// A page that already carries `editUrl` keeps it: an author who wrote one by
/// hand meant it, and `false` there is how a page opts out.
function addEditUrl(text, url) {
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) return text;
  const frontmatterEnd = text.indexOf('\n---\n', 4);
  if (frontmatterEnd < 0) return text;
  const frontmatter = text.slice(0, frontmatterEnd);
  if (/^editUrl\s*:/m.test(frontmatter)) return text;
  // The banner block above ends with its own newline, so trim before joining
  // or the frontmatter gains a blank line on every sync.
  const head = text.slice(0, frontmatterEnd).replace(/\n+$/, '');
  return `${head}\neditUrl: "${url}"${text.slice(frontmatterEnd)}`;
}

function addBanner(text, banner) {
  if (!banner) return text;
  const bannerYaml = `banner:\n  content: "${banner.replaceAll('"', '\\"')}"\n`;
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) return text;
  const frontmatterEnd = text.indexOf('\n---\n', 4);
  if (frontmatterEnd < 0) return text;
  const frontmatter = text.slice(0, frontmatterEnd);
  if (/^banner\s*:/m.test(frontmatter)) return text;
  return `${text.slice(0, frontmatterEnd)}\n${bannerYaml}${text.slice(frontmatterEnd)}`;
}

// **Every version, into its own segment.** The collection is emptied once and
// then each tree is copied under `docs/<id>/`, which is what makes the route
// `/docs/next/reference/traps/` rather than `/docs/reference/traps/`.
await rm(collectionRoot, { recursive: true, force: true });

/// Everything one version needs, resolved from its entry.
const trees = versions.map((version) => ({
  version,
  source: path.join(root, version.from),
  target: path.join(collectionRoot, 'docs', version.id),
}));

for (const tree of trees) {
  const sourceFiles = await collectDocFiles(tree.source);
  const knownRoutes = new Set(
    sourceFiles.map((file) => routeForFile(file, tree.source, tree.version.id)),
  );
  // **Checked per tree, and a broken link in any of them fails the build.** An
  // old version's pages are not maintained, but they are served, and a reader
  // following a dead link out of one has no way to tell it from a bug in the
  // one they wanted.
  // **Anchors are fatal in every tree, released ones included.** They were
  // reported-not-failed for a stable tree on the reasoning that its pages are
  // nobody's to edit -- but `check-released-docs.sh` says the opposite in its
  // own words: "Drift is allowed and is reported, not failed. Documentation
  // gets corrected after a release -- that is most of why versioned trees
  // exist." A dead anchor is a correction, not a rewrite; it does not change
  // what the release documented. Three of them sat in `v0.1` printing a
  // warning on every build, which is how a warning stops being read.
  await validateDocLinks(sourceFiles, knownRoutes, tree.source, tree.version.id, true);

  await mkdir(tree.target, { recursive: true });
  // Pages only. `khora doc` keeps a `.khora-doc` record beside the reference it
  // generates, saying which pages are its to delete; it belongs to the source
  // tree and not to the site, and a non-page inside a content collection is at
  // best noise and at worst something Astro tries to parse.
  await cp(tree.source, tree.target, {
    recursive: true,
    filter: (from) => !path.basename(from).startsWith('.'),
  });
  tree.knownRoutes = knownRoutes;
}

/// Where an unversioned `/docs/...` path should land.
///
/// **Every page, not just the section roots.** The links already written down
/// in other people's bookmarks, issues and answers were written before there
/// was a version segment, and they are deep: `/docs/reference/traps/`, not
/// `/docs/reference/`. A redirect map covering only the roots leaves every one
/// of them on a 404, which is the failure this whole change exists to avoid.
///
/// Generated rather than hand-written, from the routes that actually exist in
/// the current version, so a page added tomorrow is redirectable without
/// anybody remembering to add a line.
async function writeUnversionedRedirects(tree) {
  const map = {};
  for (const route of tree.knownRoutes) {
    const withoutVersion = `/docs/${route.slice(`/docs/${tree.version.id}/`.length)}`;
    // `/docs/` itself is written by hand in the config, beside the other short
    // paths, because it is the one people type rather than follow.
    if (withoutVersion === '/docs/') continue;
    // Astro matches these without the trailing slash.
    map[withoutVersion.replace(/\/$/, '')] = route;
  }
  const file = path.join(root, 'src', 'generated', 'redirects.js');
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(
    file,
    '// Written by scripts/sync-docs.mjs. Not edited by hand, not committed.\n'
      + `export const unversioned = ${JSON.stringify(map, null, 2)};\n`,
    'utf8',
  );
  return Object.keys(map).length;
}

const redirected = await writeUnversionedRedirects(
  trees.find((tree) => tree.version.id === current) ?? trees[0],
);

async function normalizeMarkdown(dir, tree) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      await normalizeMarkdown(full, tree);
      continue;
    }

    if (!isDocFile(entry.name)) continue;

    const relative = path.relative(tree.target, full);
    const canonicalSource = path.join(tree.source, relative);
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

    text = rewriteDocLinks(text, canonicalSource, tree.knownRoutes, tree.source, tree.version.id);
    text = addBanner(text, bannerFor(tree.version));
    text = addEditUrl(text, editUrlFor(tree.version, relative.split(path.sep).join('/')));
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

/// The id Astro will give a heading, so a link written to one lands on it.
///
/// **This has to agree with `github-slugger`**, which is what Astro's markdown
/// pipeline uses, and it did not. Two divergences, both of which produced dead
/// anchors on pages this script writes:
///
///   - `_` was stripped along with the other markdown emphasis characters, so
///     `### bounded_nursery` was indexed as `#boundednursery` while its real id
///     is `#bounded_nursery`. Every underscored name in an "API at a glance"
///     row -- which is most of `std` -- was a link to nothing.
///   - repeated hyphens were collapsed, so a heading whose punctuation leaves
///     two spaces (`` `[toolchain]` -- which Khora builds this ``, an em dash
///     between them) was indexed as `#toolchain-which-...` where the real id
///     keeps both hyphens.
///
/// So: lower-case, drop the characters a slugger drops *without* putting
/// anything in their place, turn each remaining space into a hyphen, and leave
/// the result alone. Backticks and `*`/`~` fall out of the punctuation class
/// on their own; `_` and `-` survive because a slug keeps them.
function anchorFor(heading) {
  return heading
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s\-_]/gu, '')
    .replace(/\s/g, '-');
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

for (const tree of trees) {
  await normalizeMarkdown(tree.target, tree);
  await enhanceGeneratedApi(path.join(tree.target, 'stdlib', 'api'));
}

const built = await writeProvenance();
console.log(
  `Synced ${trees.length} documentation version(s): `
    + trees.map((tree) => `${tree.version.id} <- ${tree.version.from}`).join(', '),
);
console.log(`/docs/ serves ${current}, with ${redirected} unversioned path(s) redirected into it`);
console.log(
  built.short
    ? `Built from ${built.short}${built.release ? ` (${built.release})` : ''} at ${built.builtAt}`
    : 'No revision available; the footer will not claim one',
);
