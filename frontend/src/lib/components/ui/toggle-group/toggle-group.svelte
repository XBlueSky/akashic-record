<script lang="ts">
	import { ToggleGroup as ToggleGroupPrimitive } from "bits-ui";
	import { setContext } from "svelte";
	import { cn } from "$lib/utils.js";
	import type { ToggleVariant, ToggleSize } from "./toggle-variants.js";

	let {
		ref = $bindable(null),
		value = $bindable(),
		class: className,
		variant = "default",
		size = "default",
		...restProps
	}: ToggleGroupPrimitive.RootProps & {
		variant?: ToggleVariant;
		size?: ToggleSize;
	} = $props();

	// Canonical shadcn-svelte: propagate variant/size to items via context so
	// each ToggleGroupItem applies the matching toggleVariants classes. These
	// are NOT bits-ui props, so they are intentionally excluded from restProps.
	setContext<{ variant: () => ToggleVariant; size: () => ToggleSize }>("toggle-group", {
		variant: () => variant,
		size: () => size,
	});
</script>

<!--
 `value` is destructured to make it `$bindable`, so the spread must be cast
 with `as any` per the official bits-ui guidance — otherwise the
 `"single" | "multiple"` discriminated union of `value` (`string | string[]`)
 produces svelte-check's "union type too complex to represent" error. The
 binding itself stays fully type-safe at the call site.
-->
<!-- eslint-disable @typescript-eslint/no-explicit-any -- vendored shadcn-svelte/bits-ui pattern, see comment above -->
<ToggleGroupPrimitive.Root
	bind:ref
	bind:value
	data-slot="toggle-group"
	class={cn(
		"group/toggle-group flex w-fit items-center gap-1",
		className
	)}
	{...restProps as any}
/>
<!-- eslint-enable @typescript-eslint/no-explicit-any -->
