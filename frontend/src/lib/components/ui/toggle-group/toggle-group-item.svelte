<script lang="ts">
	import { ToggleGroup as ToggleGroupPrimitive } from "bits-ui";
	import { getContext } from "svelte";
	import { cn } from "$lib/utils.js";
	import { toggleVariants, type ToggleVariant, type ToggleSize } from "./toggle-variants.js";

	let {
		ref = $bindable(null),
		class: className,
		variant,
		size,
		...restProps
	}: ToggleGroupPrimitive.ItemProps & {
		variant?: ToggleVariant;
		size?: ToggleSize;
	} = $props();

	// Inherit variant/size from the parent ToggleGroup context (canonical
	// shadcn-svelte behaviour); per-item props override when explicitly set.
	const ctx = getContext<{ variant: () => ToggleVariant; size: () => ToggleSize } | undefined>(
		"toggle-group"
	);
	const resolvedVariant = $derived(variant ?? ctx?.variant() ?? "default");
	const resolvedSize = $derived(size ?? ctx?.size() ?? "default");
</script>

<ToggleGroupPrimitive.Item
	bind:ref
	data-slot="toggle-group-item"
	data-variant={resolvedVariant}
	data-size={resolvedSize}
	class={cn(
		toggleVariants({ variant: resolvedVariant, size: resolvedSize }),
		"min-w-0 flex-1 shrink-0 rounded-md shadow-none",
		className
	)}
	{...restProps}
/>
