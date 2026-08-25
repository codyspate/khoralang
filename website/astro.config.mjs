import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://khoralang.com',
  base: '/docs',
  output: 'static',
  integrations: [
    starlight({
      title: 'Khora',
      description: 'Khora language documentation',
      social: {
        github: 'https://github.com/codyspate/khoralang',
      },
      sidebar: [
        { label: 'Getting Started', autogenerate: { directory: 'getting-started' } },
        { label: 'Guide', autogenerate: { directory: 'guide' } },
        { label: 'Language Reference', autogenerate: { directory: 'reference' } },
        { label: 'Standard Library', autogenerate: { directory: 'stdlib' } },
        { label: 'Cookbook', autogenerate: { directory: 'cookbook' } },
        { label: 'Deployment', autogenerate: { directory: 'deployment' } },
        { label: 'Migration', autogenerate: { directory: 'migration' } },
        { label: 'Limitations', autogenerate: { directory: 'limitations' } },
        { label: 'Project', autogenerate: { directory: 'project' } },
      ],
      editLink: {
        baseUrl: 'https://github.com/codyspate/khoralang/edit/main/website/content/docs/',
      },
    }),
  ],
});
