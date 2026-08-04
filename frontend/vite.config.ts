/// <reference types="vitest/config" />
import { sveltekit } from '@sveltejs/kit/vite';
import { svelteTesting } from '@testing-library/svelte/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  // sveltekit() supplies the Svelte compiler plugin — do NOT also add
  // svelte() (that is for non-Kit Vite apps; it would double-compile).
  // svelteTesting() wires @testing-library/svelte v5 auto-cleanup for Vitest.
  plugins: [tailwindcss(), sveltekit(), svelteTesting()],
  server: {
    port: 3000,
    allowedHosts: true,
    // Dev-only proxy so the SPA can call the axum backend without CORS. Mirrors
    // nginx's prod reverse proxy. Used by the frontend-e2e CI job (which runs
    // `vite dev` and hits /api + /auth through this). Target is overridable via
    // VITE_API_PROXY for local stacks on a non-default port (e.g. when another
    // service squats :8081).
    proxy: {
      '/api': { target: process.env.VITE_API_PROXY ?? 'http://localhost:8081', changeOrigin: true },
      '/auth': { target: process.env.VITE_API_PROXY ?? 'http://localhost:8081', changeOrigin: true },
      '/llms.txt': { target: process.env.VITE_API_PROXY ?? 'http://localhost:8081', changeOrigin: true },
      '^/docs/[^/]+/(llms\\.txt|llms-full\\.txt|skill\\.md)$': { target: process.env.VITE_API_PROXY ?? 'http://localhost:8081', changeOrigin: true },
    },
  },
  test: {
    environment: 'jsdom',
    // Playwright e2e specs use a different `test` import and must not be
    // collected by Vitest. Build output + Kit's generated dir excluded too.
    exclude: ['node_modules/**', 'build/**', '.svelte-kit/**', 'tests/e2e/**'],
    passWithNoTests: true,
  },
});
