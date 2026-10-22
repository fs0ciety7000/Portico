// Motion durations for Svelte transitions, read from the design tokens.
//
// Svelte animates with the Web Animations API, which the reduced-motion
// rule in base.css (CSS animations/transitions only) does not reach: every
// transition takes its duration from `dur()`, which returns 0 when the user
// asked for reduced motion.

const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");

export type Speed = "fast" | "base" | "slow";

/** How long a "Copied" confirmation stays on its button, in ms. */
export const COPIED_FEEDBACK_MS = 1600;

export function dur(speed: Speed): number {
  if (reduced.matches) return 0;
  const raw = getComputedStyle(document.documentElement).getPropertyValue(`--motion-${speed}`);
  const ms = Number.parseFloat(raw);
  return Number.isFinite(ms) ? ms : 0;
}
