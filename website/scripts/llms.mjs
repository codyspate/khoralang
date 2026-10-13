import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { current, documentationSections, stable } from '../versions.mjs';

const origin = 'https://khoralang.com';

function frontmatterOf(text) {
  const match = text.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n/);
  if (!match) return { metadata: '', body: text };
  return { metadata: match[1], body: text.slice(match[0].length) };
}

function field(metadata, name) {
  const raw = metadata.match(new RegExp(`^${name}:\\s*(.*)$`, 'm'))?.[1]?.trim();
  if (!raw) return null;
  if (raw.startsWith('"') && raw.endsWith('"')) {
    try { return JSON.parse(raw); } catch { return raw.slice(1, -1); }
  }
  return raw.replace(/^'|'$/g, '');
}

function plain(text) {
  // Keep `<...>` and `_`: they are type arguments and names (`KHORA_*`), not markup.
  return text.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/`/g, '')
    .replace(/\*\*|__/g, '')
    .replace(/\s+/g, ' ').trim();
}

function descriptionFor(page) {
  const paragraph = page.body.split(/\r?\n\s*\r?\n/)
    .find((part) => part.trim() && !/^(?:#|<!--|```|~~~|\||[-*] )/.test(part.trim())) ?? '';
  const text = plain(field(page.metadata, 'description') ?? paragraph);
  return text.match(/^.*?[.!?](?=\s|$)/)?.[0] ?? text;
}

// A fenced example can contain YAML delimiters, imports, or angle brackets;
// only the prose outside it is flattened. Otherwise the all-in-one file
// silently changes the very examples an agent is likely to copy.
//
// **No tag stripping.** The docs are plain Markdown, so every `<...>` in prose
// is a Khora type argument (`Option<A>`) or a command placeholder
// (`khora std search <query>`). Removing "tags" deleted those and taught
// readers wrong signatures. Only HTML comments go, because those are notes to
// the maintainer and can span lines.
function readableBody(body) {
  let fenced = false;
  let comment = false;
  return body.split(/\r?\n/).map((line) => {
    if (comment) {
      const end = line.indexOf('-->');
      if (end === -1) return '';
      comment = false;
      line = line.slice(end + 3);
    }
    if (/^\s*(```|~~~)/.test(line)) { fenced = !fenced; return line; }
    if (fenced) return line;
    if (/^\s*import\s+.*\s+from\s+['"]/.test(line)) return '';
    line = line.replace(/<!--.*?-->/g, '');
    const open = line.indexOf('<!--');
    if (open !== -1) { comment = true; line = line.slice(0, open); }
    return line.replace(/^\s*\{\/\*.*?\*\/\}\s*$/, '');
  }).join('\n').replace(/\n{3,}/g, '\n\n').trim();
}

function orderOf(page) {
  return Number(page.metadata.match(/^\s+order:\s*(\d+)/m)?.[1] ?? 9999);
}

function sorted(pages) {
  return pages.sort((a, b) =>
    orderOf(a) - orderOf(b) || a.relative.localeCompare(b.relative, 'en'),
  );
}

function orderedSections(pages) {
  const remaining = new Set(pages);
  const take = (predicate) => sorted(pages.filter((page) => {
    if (!remaining.has(page) || !predicate(page)) return false;
    remaining.delete(page);
    return true;
  }));
  const groups = [['Overview', take((page) => page.relative === 'index.md' || page.relative === 'index.mdx')]];
  for (const [label, directory] of documentationSections) {
    if (directory === 'stdlib') {
      groups.push([label, take((page) => page.relative.startsWith('stdlib/')
        && !page.relative.startsWith('stdlib/api/'))]);
    } else {
      groups.push([label, take((page) => page.relative.startsWith(`${directory}/`))]);
    }
  }
  groups.push(['Other', take((page) => !page.relative.startsWith('stdlib/api/'))]);
  groups.push(['Optional', take((page) => page.relative.startsWith('stdlib/api/'))]);
  return groups.filter(([, entries]) => entries.length);
}

function sourceUrl(route) { return `${origin}${route}index.md`; }

function fullText(groups) {
  return groups.flatMap(([, pages]) => pages.map((page) =>
    `# ${page.title}\n\nSource: ${sourceUrl(page.route)}\n\n${readableBody(page.body)}\n`,
  )).join('\n');
}

function indexText(version, summary, groups, includeGuide) {
  const release = stable ? `${stable.label} (${stable.cutFrom})` : 'next';
  const versionNote = version.id === 'next'
    ? `This index covers the unreleased compiler. The current stable release is ${release}. `
      + `Read [stable](${origin}/docs/${current}/llms.txt) or [next](${origin}/docs/next/llms.txt).`
    : `This index covers ${version.label}${version.id === current ? ', the current stable release' : ''}. `
      + (version.id === current ? '' : `The current stable release is ${release}. `)
      + `Next covers the unreleased compiler. `
      + `Read [stable](${origin}/docs/${current}/llms.txt) or [next](${origin}/docs/next/llms.txt).`;
  const lines = ['# Khora', '', `> ${summary}`, '', versionNote, '',
    `The [complete documentation](${origin}/docs/${version.id}/llms-full.txt) is also available in one file.`,
  ];
  if (includeGuide) {
    lines.push(`For a smaller file without the generated standard-library API, read the [guide](${origin}/docs/${version.id}/llms-full-guide.txt).`);
  }
  lines.push('');
  for (const [label, pages] of groups) {
    lines.push(`## ${label}`, '');
    for (const page of pages) lines.push(`- [${page.title}](${sourceUrl(page.route)}): ${descriptionFor(page)}`);
    lines.push('');
  }
  return `${lines.join('\n').trimEnd()}\n`;
}

export async function writeLlmDocs(trees, publicRoot, routeForFile, rewriteDocLinks) {
  // The source is always reread. A previous sync must not influence the next
  // one, and removing a page must remove its old Markdown URL as well.
  await rm(path.join(publicRoot, 'docs'), { recursive: true, force: true });
  const outputs = new Map();
  for (const tree of trees) {
    const pages = [];
    for (const file of tree.sourceFiles) {
      const relative = path.relative(tree.source, file).split(path.sep).join('/');
      const original = await readFile(file, 'utf8');
      const rewritten = rewriteDocLinks(original, file, tree.knownRoutes, tree.source, tree.version.id);
      const { metadata, body } = frontmatterOf(rewritten);
      const route = routeForFile(file, tree.source, tree.version.id);
      const title = field(metadata, 'title') ?? body.match(/^#\s+(.+)$/m)?.[1]
        ?? path.basename(relative, path.extname(relative));
      const page = { relative, route, metadata, body, title };
      pages.push(page);
      const destination = path.join(publicRoot, route.slice(1), 'index.md');
      await mkdir(path.dirname(destination), { recursive: true });
      await writeFile(destination, rewritten, 'utf8');
    }
    const groups = orderedSections(pages);
    const indexPage = pages.find((page) => page.relative === 'index.md' || page.relative === 'index.mdx');
    if (!indexPage) throw new Error(`No documentation index for ${tree.version.id}`);
    const summary = plain(indexPage.body.match(/Khora's bet is[\s\S]*?(?=\n\n)/)?.[0]
      ?? field(indexPage.metadata, 'description'));
    const full = fullText(groups);
    const guide = fullText(groups.filter(([label]) => label !== 'Optional'));
    const includeGuide = Buffer.byteLength(full) > 500_000;
    const index = indexText(tree.version, summary, groups, includeGuide);
    const destination = path.join(publicRoot, 'docs', tree.version.id);
    await writeFile(path.join(destination, 'llms.txt'), index, 'utf8');
    await writeFile(path.join(destination, 'llms-full.txt'), full, 'utf8');
    if (includeGuide) {
      await writeFile(path.join(destination, 'llms-full-guide.txt'), guide, 'utf8');
    }
    outputs.set(tree.version.id, { index, full, guide, count: pages.length });
  }
  const latest = outputs.get(current);
  await writeFile(path.join(publicRoot, 'llms.txt'), latest.index, 'utf8');
  await writeFile(path.join(publicRoot, 'llms-full.txt'), latest.full, 'utf8');
  await rm(path.join(publicRoot, 'llms-full-guide.txt'), { force: true });
  if (Buffer.byteLength(latest.full) > 500_000) {
    await writeFile(path.join(publicRoot, 'llms-full-guide.txt'), latest.guide, 'utf8');
  }
  return outputs;
}
