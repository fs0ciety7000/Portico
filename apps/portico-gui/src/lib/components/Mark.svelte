<script lang="ts">
  // The portico mark (design/portico-mark.svg), inline so it takes the
  // theme's accent instead of a fixed brass.
  let { size = 18 }: { size?: number } = $props();
</script>

<svg viewBox="96 130 320 302" width={size} height={size} aria-hidden="true">
  <g fill="none" stroke="var(--accent)" stroke-width="22" stroke-linecap="square" stroke-linejoin="miter">
    <path d="M116 150 H396" />
    <path d="M136 186 H376" />
    <path d="M160 186 V392" />
    <path d="M352 186 V392" />
    <path d="M208 392 V290 A48 48 0 0 1 304 290 V392" />
    <path d="M116 412 H396" />
  </g>
</svg>
