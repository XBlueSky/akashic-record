import { includeIgnoreFile } from "@eslint/compat";
import js from "@eslint/js";
import prettier from "eslint-config-prettier";
import svelte from "eslint-plugin-svelte";
import globals from "globals";
import { fileURLToPath } from "node:url";
import ts from "typescript-eslint";
import svelteConfig from "./svelte.config.js";

const gitignorePath = fileURLToPath(new URL("./.gitignore", import.meta.url));

export default ts.config(
	includeIgnoreFile(gitignorePath),
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs.recommended,
	prettier,
	...svelte.configs.prettier,
	{
		languageOptions: {
			globals: { ...globals.browser, ...globals.node }
		},
		rules: {
			// Repo convention: a leading underscore marks a binding that is
			// intentionally unused (each-block placeholders, required-but-unused
			// callback params, etc.) — pre-existing pattern across the codebase
			// (e.g. `as _i`, `_config: FullConfig`), not something introduced here.
			"@typescript-eslint/no-unused-vars": [
				"error",
				{
					args: "after-used",
					argsIgnorePattern: "^_",
					varsIgnorePattern: "^_",
					caughtErrorsIgnorePattern: "^_",
					destructuredArrayIgnorePattern: "^_"
				}
			]
		}
	},
	{
		files: ["**/*.svelte", "**/*.svelte.ts", "**/*.svelte.js"],
		languageOptions: {
			parserOptions: {
				extraFileExtensions: [".svelte"],
				parser: ts.parser,
				svelteConfig
			}
		},
		rules: {
			// Svelte 5 runes idiom: referencing a value as a bare expression
			// statement inside `$effect(() => { dep; sideEffect(); })` registers
			// it as a tracked dependency (often paired with `untrack()` for the
			// rest of the body). ESLint's generic JS rule doesn't know about this
			// and flags the bare reference as a useless expression.
			"@typescript-eslint/no-unused-expressions": "off",
			// This repo builds plain string paths for goto()/href everywhere
			// (SPA-mode adapter-static app, no typed route IDs in use); adopting
			// SvelteKit's resolve() helper app-wide is a routing migration, not a
			// lint-tooling change, so it's out of scope here.
			"svelte/no-navigation-without-resolve": "off",
			// Every flagged Map/Set here is a locally-built scratch structure
			// (populated once, then returned/reassigned wholesale) rather than a
			// live $state container mutated in place — the one case SvelteMap /
			// SvelteSet actually matters for. The rule can't tell the difference.
			"svelte/prefer-svelte-reactivity": "off"
		}
	}
);
