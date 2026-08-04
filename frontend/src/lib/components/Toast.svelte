<script lang="ts">
  import { toast } from '$lib/state/toast.svelte';
  import { AlertTriangle, XCircle, Info, X } from '@lucide/svelte';

  const iconMap = {
    warning: AlertTriangle,
    error: XCircle,
    info: Info,
  } as const;

  const accentMap = {
    warning: "border-l-amber-500 text-amber-400",
    error: "border-l-red-500 text-red-400",
    info: "border-l-blue-500 text-blue-400",
  } as const;

  const actionColorMap = {
    warning: "text-amber-300 hover:text-amber-200",
    error: "text-red-300 hover:text-red-200",
    info: "text-blue-300 hover:text-blue-200",
  } as const;

  $effect(() => {
    const currentToasts = toast.items;
    const timers: ReturnType<typeof setTimeout>[] = [];

    for (const t of currentToasts) {
      const timer = setTimeout(() => {
        toast.dismiss(t.id);
      }, 8000);
      timers.push(timer);
    }

    return () => {
      for (const timer of timers) {
        clearTimeout(timer);
      }
    };
  });
</script>

{#if toast.items.length > 0}
  <div class="fixed bottom-4 right-4 z-50 flex flex-col-reverse gap-3 max-w-[400px]">
    {#each toast.items as item (item.id)}
      {@const Icon = iconMap[item.type]}
      <div
        class="flex items-start gap-3 rounded-lg border border-border/40 border-l-4 bg-background/80 p-4 shadow-lg backdrop-blur-md animate-slide-up {accentMap[item.type]}"
        role="alert"
      >
        <Icon class="mt-0.5 h-5 w-5 shrink-0" />

        <div class="flex-1 min-w-0">
          <p class="text-sm text-foreground">{item.message}</p>

          {#if item.action}
            <button
              type="button"
              class="mt-1.5 text-sm font-medium underline underline-offset-2 {actionColorMap[item.type]} transition-colors"
              onclick={item.action.onClick}
            >
              {item.action.label}
            </button>
          {/if}
        </div>

        <button
          type="button"
          class="shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:text-foreground"
          onclick={() => toast.dismiss(item.id)}
          aria-label="Dismiss"
        >
          <X class="h-4 w-4" />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  @keyframes slide-up {
    from {
      opacity: 0;
      transform: translateY(1rem);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  :global(.animate-slide-up) {
    animation: slide-up 0.25s ease-out;
  }
</style>
