import { defineCollection } from 'astro:content';
import { docsLoader } from '@astrojs/starlight/loaders';
import { docsSchema } from '@astrojs/starlight/schema';

/// A page's id, which is also its route.
///
/// **Astro's default slugifies every path segment, and a slugifier eats dots.**
/// So `docs/v0.1/reference/traps.md` was served at `/docs/v01/reference/traps/`
/// while `versions.mjs` said the version was `v0.1` — and two files disagreeing
/// about which versions exist is the one failure that list was written to
/// prevent. It fails quietly in both directions: the link checker passes,
/// because it reads the list, and the site 404s, because it serves the routes.
/// The redirect from `/docs/` pointed at the id, so the front door was the
/// first thing broken.
///
/// The dot is the only thing that needed rescuing. Every page here is written
/// to a path that is already a slug — lower case, digits, hyphens — so this
/// keeps the path rather than re-deriving it, and refuses anything that would
/// have needed slugifying. A file added as `Getting Started.md` should stop the
/// build rather than quietly serve a route nobody linked to.
function generateId({ entry }: { entry: string }): string {
  const withoutExtension = entry.replace(/\\/g, '/').replace(/\.(md|mdx)$/, '');

  for (const segment of withoutExtension.split('/')) {
    if (!/^[a-z0-9.-]+$/.test(segment)) {
      throw new Error(
        `The page \`${entry}\` has a path segment \`${segment}\` that is not already a URL slug.\n` +
          'Rename it to lower case, digits and hyphens. This project does not slugify paths, ' +
          'because slugifying is what removed the dot from the `v0.1` documentation tree.',
      );
    }
  }

  return withoutExtension.replace(/\/index$/, '').replace(/^index$/, '');
}

export const collections = {
  docs: defineCollection({ loader: docsLoader({ generateId }), schema: docsSchema() }),
};
