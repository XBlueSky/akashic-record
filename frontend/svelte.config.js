import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    // SPA mode: every route is served by the client router via a fallback
    // page. nginx `try_files ... /index.html` serves it for deep links.
    adapter: adapter({
      fallback: 'index.html',
      precompress: false,
      strict: false,
    }),
  },
};

export default config;
