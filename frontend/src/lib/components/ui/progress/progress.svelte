<script lang="ts">
	import { Progress as ProgressPrimitive } from "bits-ui";
	import { cn } from "$lib/utils.js";

	let {
		ref = $bindable(null),
		class: className,
		value = 0,
		max = 100,
		min = 0,
		...restProps
	}: ProgressPrimitive.RootProps = $props();

	// Range-correct fill: account for a non-zero `min` and clamp to [0, 100].
	// aria-value* attributes stay delegated to the bits-ui primitive below.
	const pct = $derived(
		Math.min(
			100,
			Math.max(0, (((value ?? 0) - (min ?? 0)) / ((max ?? 100) - (min ?? 0))) * 100)
		)
	);
</script>

<ProgressPrimitive.Root
	bind:ref
	data-slot="progress"
	class={cn("bg-primary/20 relative h-2 w-full overflow-hidden rounded-full", className)}
	{value}
	{max}
	{min}
	{...restProps}
>
	<div
		data-slot="progress-indicator"
		class="bg-primary h-full w-full flex-1 transition-all"
		style="transform: translateX(-{100 - pct}%)"
	></div>
</ProgressPrimitive.Root>
