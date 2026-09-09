// Which documentation trees the site serves, and which one `/docs/` means.
//
// **One list, read by both the sync script and the Astro config**, because the
// two have to agree about every route or the link checker passes and the site
// 404s. `scripts/sync-docs.mjs` writes the pages under `id`; `astro.config.mjs`
// builds the sidebar and the redirects from the same entries.
//
// # The shape, and the granularity
//
// A section per release that can change the answer to "how do I do X", plus
// `next` for the version being written. Not a section per patch: `0.1.1`
// fixing a typo gives a reader the same language, and a switcher listing forty
// entries is a switcher nobody uses.
//
// **Which release that is depends on whether the major is zero.** From 1.0,
// it is the major: `1.x` promises compatibility within itself, so one section
// covers all of it. Before 1.0, semver puts the breaking changes in the
// *minor* -- `0.1` to `0.2` is what `1` to `2` will be -- so 0.x gets a
// section per minor. One rule, applied to whichever number is allowed to
// break.
//
// This used to say there was no stable section before v1, on the argument
// that nothing pre-1.0 was worth pinning. That was right while `next` was the
// only compiler anybody could install and wrong the hour v0.1.0 published: a
// front door that sends every reader to documentation banner-marked *this
// describes a compiler you cannot install* is a front door with nothing
// behind it. `docs/design/docs-urls.md` has the argument in full.
//
// # Adding one
//
// Cut it from the tag rather than from the working tree, which is what makes
// it documentation *for that release* rather than for whatever was on `main`
// that afternoon:
//
//     git archive v0.2.0 website/content/docs | tar -x
//     mv website/content/docs website/content/versions/v0.2
//
// then add an entry below with `cutFrom` naming the tag. `current` follows the
// newest stable entry by itself, and so do the sidebar, the banners and the
// short paths.
//
// `next` keeps being written in `website/content/docs/`, and keeps being the
// tree the repository's own gates check, because it is the one that matches
// the compiler in this checkout. A released tree is not checked against this
// compiler and must not be: it describes a different one.

/// Every documentation tree, newest first.
///
/// `id` is the URL segment and the directory name. `label` is what a reader
/// sees. `stable` says whether it describes a released compiler — which decides
/// whether the page carries the unstable banner. `cutFrom` is the tag a
/// stable tree was taken from, and `next` has none because it is not taken
/// from anything.
export const versions = [
  {
    id: 'next',
    label: 'next',
    stable: false,
    /// Where the pages come from, relative to `website/`.
    ///
    /// `next` is the working tree; a released version is a copy taken at the
    /// tag. Keeping the working tree at `content/docs` rather than moving it to
    /// `content/versions/next` means every other tool in the repository —
    /// `khora doc --out`, `scripts/check-docs.sh`, the stranger reading files
    /// on disk — keeps the path it already knows.
    from: 'content/docs',
  },
  {
    id: 'v0.1',
    label: 'v0.1',
    stable: true,
    /// The tag this tree was cut from, byte for byte.
    ///
    /// Recorded rather than assumed: the claim `/docs/v0.1/` makes is that it
    /// documents the compiler `v0.1.0` publishes, and a tree copied from
    /// whatever happened to be checked out cannot make it. A later fix to a
    /// page here is allowed and expected -- documentation gets corrected after
    /// a release, which is most of why versioned trees exist -- and this stays
    /// pointing at where the tree began.
    cutFrom: 'v0.1.0',
    from: 'content/versions/v0.1',
  },
];

/// The version `/docs/` redirects to.
///
/// The newest *stable* one, or `next` while there is none. A reader who types
/// `/docs/` wants the documentation for the compiler they can install, and
/// before v1 the only compiler anybody can install is described by `next`.
export const current = versions.find((each) => each.stable)?.id ?? 'next';

/// The newest stable version, or `null` before there is one.
export const stable = versions.find((each) => each.stable) ?? null;

/// The entry for `id`, or `undefined`.
export function versionOf(id) {
  return versions.find((each) => each.id === id);
}

/// The repository these pages are written in.
///
/// One copy, read by `astro.config.mjs` for the short paths and the social
/// link and by `scripts/sync-docs.mjs` for every page's "Edit this page" link.
/// Two copies is how a fork ends up sending editors to somebody else's tree.
export const repository = 'https://github.com/codyspate/khoralang';

/// The branch an edit is opened against.
export const editBranch = 'main';

/// Where to edit the page that `relativePath` inside `version`'s tree renders.
///
/// **Per page, because no single base URL can be right.** Starlight builds an
/// edit link by appending the entry's id within the collection, and that id is
/// `docs/next/reference/traps.md` or `docs/v0.1/reference/traps.md` -- a path
/// that exists only in the generated tree, which `website/.gitignore` keeps out
/// of the repository entirely. The real sources are
/// `website/content/docs/reference/traps.md` and
/// `website/content/versions/v0.1/reference/traps.md`: two different roots,
/// neither of them a suffix of the other, so one `editLink.baseUrl` cannot
/// reach both and the one that was configured reached neither. Every edit link
/// on the site was a 404.
///
/// `from` already records where each tree comes from, so the answer is here
/// rather than guessed in the config: `sync-docs.mjs` writes the result into
/// each generated page's `editUrl` frontmatter, which Starlight prefers over
/// the base URL.
export function editUrlFor(version, relativePath) {
  const inRepo = `website/${version.from}/${relativePath}`
    .split('\\')
    .join('/')
    .replace(/\/+/g, '/');
  return `${repository}/edit/${editBranch}/${inRepo}`;
}
