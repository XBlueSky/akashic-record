<script lang="ts">
  import { fly } from "svelte/transition";
  import { Marked } from "marked";
  import DOMPurify from "dompurify";
  import { CATEGORIES, type Category, type NoteDetail } from "$lib/types/index.js";
  import { fetchNote, updateNote, isAuthError } from "$lib/api";
  import { toast } from "$lib/state/toast.svelte";
  import { t } from "svelte-i18n";

  // Module-local Marked instance — never mutates the global singleton.
  const md = new Marked({ breaks: true, gfm: true });

  interface Props {
    noteUuid: string;
    repoName?: string;
    onclose: () => void;
    onsaved: () => void;
  }

  let {
    noteUuid,
    repoName = "",
    onclose,
    onsaved,
  }: Props = $props();

  // ── State ──────────────────────────────────────────────────────────────────
  let loading = $state(true);
  let saving = $state(false);
  let error = $state("");

  let title = $state("");
  let summary = $state("");
  let content = $state("");
  let category: Category = $state("ARCHITECTURE");
  let tags: string[] = $state([]);

  let issueRef = $state("");
  let topic = $state("");
  let tagInput = $state("");
  let activeTab: "write" | "preview" = $state("write");

  let summaryLen = $derived(summary.length);

  // ── Load note when uuid / repo changes ────────────────────────────────────
  // Generation guard: if noteUuid changes while a fetch is in-flight (e.g. the
  // user clicks note A then note B in the ChunkSidebar list), A's slower
  // response must NOT clobber B's form — otherwise a subsequent Save would call
  // updateNote(repo, B_uuid, {...A_content}) and silently corrupt note B.
  // Plain `let` (not $state) — read only by async callbacks, never by markup.
  let loadGen = 0;
  async function loadNote() {
    if (!noteUuid || !repoName) return;
    const gen = ++loadGen;
    loading = true;
    error = "";
    try {
      const data: NoteDetail = await fetchNote(repoName, noteUuid);
      if (gen !== loadGen) return; // a newer note load started — discard
      title = data.title ?? "";
      summary = data.summary ?? "";
      content = data.content ?? "";
      category = (data.category as Category) ?? "ARCHITECTURE";
      tags = data.tags ?? [];
    } catch (e: unknown) {
      if (gen !== loadGen) return; // stale failure — the newer load owns the form
      if (!isAuthError(e)) {
        error = e instanceof Error ? e.message : "Failed to load note";
      }
    } finally {
      if (gen === loadGen) loading = false;
    }
  }

  $effect(() => {
    if (noteUuid && repoName) loadNote();
  });

  // ── Save ───────────────────────────────────────────────────────────────────
  async function handleSave() {
    if (!repoName) return;
    saving = true;
    error = "";
    try {
      await updateNote(repoName, noteUuid, { title, summary, content, category, tags });
      onsaved();
    } catch (e: unknown) {
      if (!isAuthError(e)) {
        const msg = e instanceof Error ? e.message : String($t("noteEditor.saveFailed") ?? "Failed to save note");
        error = msg;
        toast.push(msg, "error");
      }
    } finally {
      saving = false;
    }
  }

  // ── Tag helpers ────────────────────────────────────────────────────────────
  function addTag() {
    const val = tagInput.trim().toLowerCase();
    if (val && !tags.includes(val)) {
      tags = [...tags, val];
    }
    tagInput = "";
  }

  function removeTag(tag: string) {
    tags = tags.filter((t) => t !== tag);
  }

  function handleTagKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addTag();
    }
  }

  // ── Markdown preview (XSS boundary: ALWAYS sanitize before {@html}) ────────
  function renderMarkdown(src: string): string {
    return DOMPurify.sanitize(md.parse(src) as string);
  }
</script>

<div
  class="note-editor custom-scrollbar absolute top-0 right-0 z-10 h-full w-[420px] glass border-l border-border flex flex-col"
  transition:fly={{ x: 420, duration: 250 }}
>
  <!-- Sticky header -->
  <div class="sticky top-0 z-10 glass border-b border-border px-4 py-3 shrink-0">
    <div class="flex items-center justify-between">
      <h2 class="text-sm font-semibold text-foreground font-mono">{$t("noteEditor.editNote") || "Edit Note"}</h2>
      <button
        class="bg-transparent border border-border rounded-md text-muted-foreground text-sm w-[26px] h-[26px] flex items-center justify-center cursor-pointer hover:border-white/15 hover:text-foreground transition-colors shrink-0"
        onclick={onclose}
      >x</button>
    </div>
  </div>

  {#if loading}
    <div class="flex items-center justify-center py-12 flex-1">
      <p class="text-sm text-muted-foreground font-mono">{$t("common.loading") || "Loading…"}</p>
    </div>
  {:else if error && !title}
    <div class="flex items-center justify-center py-12 flex-1">
      <p class="text-sm text-red-500 font-mono">{error}</p>
    </div>
  {:else}
    <div class="flex flex-col flex-1 overflow-y-auto custom-scrollbar">
      <div class="px-4 py-3 space-y-3 flex flex-col flex-1">
        <!-- Title -->
        <div class="space-y-1">
          <label for="note-title" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">{$t("noteEditor.title") || "Title"}</label>
          <input
            id="note-title"
            type="text"
            bind:value={title}
            class="w-full bg-transparent border border-border rounded-md px-3 py-1.5 text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:border-blue-500/50 transition-colors"
            placeholder={String($t("noteEditor.titlePlaceholder") ?? "Note title…")}
          />
        </div>

        <!-- Category -->
        <div class="space-y-1">
          <label for="note-category" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">{$t("noteEditor.category") || "Category"}</label>
          <select
            id="note-category"
            bind:value={category}
            class="w-full bg-transparent border border-border rounded-md px-3 py-1.5 text-sm text-foreground font-mono focus:outline-none focus:border-blue-500/50 transition-colors cursor-pointer"
          >
            {#each CATEGORIES as cat (cat)}
              <option value={cat} class="bg-[#09090b] text-foreground">{cat.replace("_", " ")}</option>
            {/each}
          </select>
        </div>

        <!-- Issue Ref (read-only — saga assignment via MCP tools) -->
        <div class="space-y-1">
          <label for="note-issue-ref" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">
            Issue Ref
            <span class="text-[10px] text-slate-600 normal-case tracking-normal ml-1" title="Saga assignment is handled automatically via MCP tools (save_note)">MCP only</span>
          </label>
          <input
            id="note-issue-ref"
            type="text"
            value={issueRef}
            readonly
            class="w-full bg-white/[0.02] border border-border rounded-md px-3 py-1.5 text-sm text-muted-foreground font-mono cursor-not-allowed"
            placeholder="PROJ-123456"
          />
        </div>

        <!-- Topic / Saga (read-only — saga assignment via MCP tools) -->
        <div class="space-y-1">
          <label for="note-topic" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">
            Topic / Saga
            <span class="text-[10px] text-slate-600 normal-case tracking-normal ml-1" title="Saga assignment is handled automatically via MCP tools (save_note)">MCP only</span>
          </label>
          <input
            id="note-topic"
            type="text"
            value={topic}
            readonly
            class="w-full bg-white/[0.02] border border-border rounded-md px-3 py-1.5 text-sm text-muted-foreground font-mono cursor-not-allowed"
            placeholder="auth-migration"
          />
        </div>

        <!-- Summary -->
        <div class="space-y-1">
          <div class="flex items-center justify-between">
            <label for="note-summary" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">{$t("noteEditor.summary") || "Summary"}</label>
            <span class="text-[10px] font-mono {summaryLen > 100 ? 'text-red-400' : 'text-muted-foreground'}">{summaryLen}/100</span>
          </div>
          <input
            id="note-summary"
            type="text"
            bind:value={summary}
            maxlength={100}
            class="w-full bg-transparent border border-border rounded-md px-3 py-1.5 text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none focus:border-blue-500/50 transition-colors"
            placeholder={String($t("noteEditor.summaryPlaceholder") ?? "One-line summary…")}
          />
        </div>

        <!-- Tab bar -->
        <div class="flex gap-1 border-b border-border">
          <button
            class="px-3 py-1.5 text-xs font-mono cursor-pointer bg-transparent border-none transition-colors {activeTab === 'write' ? 'text-foreground border-b-2 border-b-blue-500' : 'text-muted-foreground hover:text-foreground'}"
            onclick={() => (activeTab = "write")}
          >{$t("noteEditor.write") || "Write"}</button>
          <button
            class="px-3 py-1.5 text-xs font-mono cursor-pointer bg-transparent border-none transition-colors {activeTab === 'preview' ? 'text-foreground border-b-2 border-b-blue-500' : 'text-muted-foreground hover:text-foreground'}"
            onclick={() => (activeTab = "preview")}
          >{$t("noteEditor.preview") || "Preview"}</button>
        </div>

        <!-- Content area -->
        <div class="flex-1 min-h-[200px] flex flex-col">
          {#if activeTab === "write"}
            <textarea
              bind:value={content}
              class="w-full flex-1 bg-transparent border border-border rounded-md px-3 py-2 text-sm text-foreground font-mono placeholder:text-muted-foreground resize-none focus:outline-none focus:border-blue-500/50 transition-colors"
              placeholder={String($t("noteEditor.contentPlaceholder") ?? "Write your note in Markdown…")}
            ></textarea>
          {:else}
            <div class="flex-1 border border-border rounded-md px-3 py-2 text-sm text-foreground overflow-y-auto note-prose">
              {#if content.trim()}
                <!-- XSS boundary: renderMarkdown() always DOMPurify.sanitizes before {@html} -->
                {@html renderMarkdown(content)}
              {:else}
                <p class="text-muted-foreground italic">{$t("noteEditor.nothingToPreview") || "Nothing to preview."}</p>
              {/if}
            </div>
          {/if}
        </div>

        <!-- Tags -->
        <div class="space-y-1.5">
          <label for="note-tag-input" class="text-xs font-mono text-muted-foreground uppercase tracking-wide">{$t("noteEditor.tags") || "Tags"}</label>
          <div class="flex flex-wrap gap-1.5 items-center">
            {#each tags as tag (tag)}
              <span class="inline-flex items-center gap-1 bg-white/5 border border-border rounded-full px-2.5 py-0.5 text-xs font-mono text-foreground">
                {tag}
                <button
                  class="text-muted-foreground hover:text-red-400 bg-transparent border-none cursor-pointer text-xs leading-none p-0 ml-0.5 transition-colors"
                  onclick={() => removeTag(tag)}
                >&times;</button>
              </span>
            {/each}
            <input
              id="note-tag-input"
              type="text"
              bind:value={tagInput}
              onkeydown={handleTagKeydown}
              class="bg-transparent border-none text-sm text-foreground font-mono placeholder:text-muted-foreground focus:outline-none min-w-[80px] flex-1 py-0.5"
              placeholder={String($t("noteEditor.tagPlaceholder") ?? "Add tag…")}
            />
          </div>
        </div>

        <!-- Error message -->
        {#if error}
          <p class="text-xs text-red-400 font-mono">{error}</p>
        {/if}
      </div>
    </div>

    <!-- Footer -->
    <div class="sticky bottom-0 glass border-t border-border px-4 py-3 flex items-center gap-2 shrink-0">
      <div class="flex-1"></div>
      <button
        class="px-3 py-1.5 text-xs font-mono rounded-md border border-border text-muted-foreground bg-transparent cursor-pointer hover:text-foreground hover:border-white/15 transition-colors"
        onclick={onclose}
      >{$t("noteEditor.cancel") || "Cancel"}</button>
      <button
        class="px-3 py-1.5 text-xs font-mono rounded-md border border-blue-500/40 text-blue-400 bg-blue-500/10 cursor-pointer hover:bg-blue-500/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        onclick={handleSave}
        disabled={saving}
      >{saving ? ($t("noteEditor.saving") || "Saving…") : ($t("noteEditor.save") || "Save")}</button>
    </div>
  {/if}
</div>

<style>
  /* Dark-theme scrollbar */
  .custom-scrollbar::-webkit-scrollbar {
    width: 6px;
  }
  .custom-scrollbar::-webkit-scrollbar-track {
    background: transparent;
  }
  .custom-scrollbar::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.08);
    border-radius: 3px;
  }
  .custom-scrollbar::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.15);
  }
  .custom-scrollbar {
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.08) transparent;
  }

  /* Note prose styles for preview */
  :global(.note-prose h1) { font-size: 1.25em; font-weight: 700; margin: 0.8em 0 0.4em; color: #e2e8f0; }
  :global(.note-prose h2) { font-size: 1.1em; font-weight: 600; margin: 0.7em 0 0.3em; color: #e2e8f0; }
  :global(.note-prose h3) { font-size: 1em; font-weight: 600; margin: 0.6em 0 0.2em; color: #cbd5e1; }
  :global(.note-prose ul) { list-style: disc; padding-left: 1.5em; margin: 0.4em 0; }
  :global(.note-prose ol) { list-style: decimal; padding-left: 1.5em; margin: 0.4em 0; }
  :global(.note-prose li) { margin: 0.15em 0; }
  :global(.note-prose blockquote) { border-left: 3px solid #475569; padding-left: 0.75em; margin: 0.5em 0; color: #94a3b8; font-style: italic; }
  :global(.note-prose pre) { overflow-x: auto; border-radius: 0.375rem; border: 1px solid hsl(var(--border)); background: #09090b; padding: 0.75rem; margin: 0.5em 0; font-size: 0.8em; }
  :global(.note-prose code) { font-family: ui-monospace, monospace; }
  :global(.note-prose :not(pre) > code) { background: hsl(var(--muted)); padding: 0.15em 0.35em; border-radius: 0.25rem; font-size: 0.9em; color: #60a5fa; }
  :global(.note-prose p) { margin: 0.4em 0; }
  :global(.note-prose a) { color: #60a5fa; text-decoration: underline; }
  :global(.note-prose table) { border-collapse: collapse; margin: 0.5em 0; width: 100%; }
  :global(.note-prose th, .note-prose td) { border: 1px solid #334155; padding: 0.35em 0.6em; font-size: 0.85em; }
  :global(.note-prose th) { background: #1e293b; font-weight: 600; }

  /* Responsive: bottom sheet on narrow viewports */
  @media (max-width: 1023px) {
    .note-editor {
      position: fixed;
      top: auto;
      bottom: 0;
      left: 0;
      right: 0;
      width: 100%;
      max-height: 80vh;
      border-left: none;
      border-top: 1px solid hsl(var(--border));
      border-radius: 12px 12px 0 0;
    }
  }
</style>
