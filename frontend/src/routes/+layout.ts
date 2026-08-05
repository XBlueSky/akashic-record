// SPA mode for the whole app: no SSR, no prerender. D3/Three.js are
// client-only; auth is a client token. adapter-static emits a fallback page.
export const ssr = false;
export const prerender = false;
export const trailingSlash = "never";
