// Which documentation trees the site serves, and which one `/docs/` means.
//
// **One list, read by both the sync script and the Astro config**, because the
// two have to agree about every route or the link checker passes and the site
// 404s. `scripts/sync-docs.mjs` writes the pages under `id`; `astro.config.mjs`
// builds the sidebar and the redirects from the same entries.
//
// # The shape, and why it is major versions
//
// A section per *stable major*, plus `next` for the version being written. Not
// a section per patch: `0.1.1` fixing a typo does not give a reader a different
// language, and a switcher listing forty entries is a switcher nobody uses.
// A major is the granularity at which the answer to "how do I do X" actually
// changes, which is what somebody is switching versions to find out.
//
// Before v1 there is no stable section at all. Everything ships from `next`,
// which is what the pre-1.0 promise in `/docs/reference/compatibility/` already
// says: the language may change, so there is nothing yet whose documentation is
// worth pinning. `docs/design/docs-urls.md` has the argument in full.
//
// # Adding one
//
// When v1 ships: copy the tree that was released into
// `website/content/versions/v1/`, add `{ id: 'v1', label: 'v1', stable: true }`
// below, and set `current` to `'v1'`. `next` keeps being written in
// `website/content/docs/`, and keeps being the tree the repository's own gates
// check, because it is the one that matches the compiler in this checkout.

/// Every documentation tree, newest first.
///
/// `id` is the URL segment and the directory name. `label` is what a reader
/// sees. `stable` says whether it describes a released compiler — which decides
/// whether the page carries the unstable banner.
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
