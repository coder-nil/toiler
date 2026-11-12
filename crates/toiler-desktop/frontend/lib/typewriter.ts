/**
 * Advances a typewriter reveal by one animation frame.
 *
 * The reveal step grows with the backlog so a burst of streamed text still
 * finishes promptly instead of lagging behind the model. When the incoming
 * preview is not a forward extension of what is shown already, the preview
 * normalization has rewritten earlier characters — snap to it rather than
 * animating backwards.
 */
export function revealNext(displayed: string, target: string): string {
  if (displayed === target) return target;
  if (!target.startsWith(displayed)) return target;
  const remaining = target.length - displayed.length;
  const step = Math.min(remaining, Math.max(2, Math.min(12, Math.ceil(remaining / 6))));
  return target.slice(0, displayed.length + step);
}
