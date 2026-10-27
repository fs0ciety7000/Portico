<script lang="ts">
  // A core message with its `code` parts (aliases, ids, paths) in mono:
  // `received host \`legacy\` is already defined`. The first word is
  // capitalized only when it is a plain word (`sentence`), never an
  // identifier. Text only: no {@html}.
  import { codeSpans, sentence } from "../api";

  let { text, capitalize = true }: { text: string; capitalize?: boolean } = $props();
  const parts = $derived(codeSpans(capitalize ? sentence(text) : text));
</script>

{#each parts as part, i (i)}{#if part.code}<span class="code">{part.text}</span>{:else}{part.text}{/if}{/each}

<style>
  .code { font-family: var(--font-mono); }
</style>
