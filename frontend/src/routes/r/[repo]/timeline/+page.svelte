<script lang="ts">
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { fetchPermissions } from "$lib/api";
  import TimelineView from "$lib/components/TimelineView.svelte";
  import type { Category } from "$lib/types/index.js";

  let perms = $state({ can_edit: false, can_delete: false });

  // Validate the `category` URL param against the real enum — never cast an
  // arbitrary query string into Category.
  const VALID_CATEGORIES = ["ARCHITECTURE", "BUG_FIX", "CONFIG", "ONBOARDING", "DECISION"] as const;
  const category = $derived.by<Category | null>(() => {
    const raw = page.url.searchParams.get("category");
    return (VALID_CATEGORIES as readonly string[]).includes(raw ?? "") ? (raw as Category) : null;
  });

  $effect(() => {
    const repo = page.params.repo ?? "";
    if (!repo) return;
    fetchPermissions(repo).then((p) => {
      perms = p;
    });
  });
</script>

<TimelineView
  repoName={page.params.repo ?? ""}
  branch={page.url.searchParams.get("branch")}
  {category}
  highlightNoteId={page.url.searchParams.get("note")}
  canEdit={perms.can_edit}
  canDelete={perms.can_delete}
  onedit={(uuid) => {
    const u = new URL(page.url.href);
    u.searchParams.set("edit", uuid);
    goto(u.pathname + u.search);
  }}
  ondeleted={() => { /* refresh handled inside TimelineView */ }}
/>
