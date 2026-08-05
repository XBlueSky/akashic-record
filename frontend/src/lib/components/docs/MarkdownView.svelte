<script lang="ts">
	import { mount, unmount } from "svelte";
	import { t } from "svelte-i18n";
	import { addToast } from "$lib/state/toast.svelte.js";
	import CodeBlock from "./CodeBlock.svelte";
	import MermaidBlock from "./MermaidBlock.svelte";

	interface Props {
		html: string;
		/** BCP-47 for the prose container (D7): zh-TW pages read correctly. */
		lang: string;
		el?: HTMLElement | null;
	}
	let { html, lang, el = $bindable(null) }: Props = $props();

	function onClick(e: MouseEvent): void {
		const a = (e.target as HTMLElement).closest("a.header-anchor");
		if (!a) return;
		const href = a.getAttribute("href") ?? "";
		navigator.clipboard?.writeText(`${location.origin}${location.pathname}${href}`);
		addToast($t("docs.anchorCopied"));
	}

	function onImgError(e: Event): void {
		const target = e.target as HTMLElement;
		if (target.tagName === "IMG") target.classList.add("docs-img-broken");
	}

	$effect(() => {
		void html; // re-run per render
		const root = el;
		if (!root) return;
		const mounted: Record<string, unknown>[] = [];
		for (const pre of root.querySelectorAll("pre[data-code]")) {
			const code = pre.querySelector("code")?.textContent ?? "";
			const lang = pre.getAttribute("data-lang") ?? "";
			const host = document.createElement("div");
			pre.replaceWith(host);
			mounted.push(mount(CodeBlock, { target: host, props: { code, lang } }));
		}
		for (const pre of root.querySelectorAll("pre[data-mermaid]")) {
			const code = pre.textContent ?? "";
			const host = document.createElement("div");
			pre.replaceWith(host);
			mounted.push(mount(MermaidBlock, { target: host, props: { code } }));
		}
		return () => {
			for (const m of mounted) void unmount(m);
		};
	});
</script>

<!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
<div
	bind:this={el}
	class="docs-prose"
	{lang}
	data-testid="docs-content"
	onclick={onClick}
	onerrorcapture={onImgError}
>
	<!-- eslint-disable-next-line svelte/no-at-html-tags -- html prop is produced by $lib/docs/markdown.ts, which always runs it through DOMPurify.sanitize() -->
	{@html html}
</div>
