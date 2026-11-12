import type { CSSProperties } from "react";
import { vs, vscDarkPlus } from "react-syntax-highlighter/dist/cjs/styles/prism";

type SyntaxTheme = Record<string, CSSProperties>;

const PRE_SELECTOR = 'pre[class*="language-"]';

/**
 * React cannot safely update a node that alternates between the `background`
 * shorthand and `backgroundColor`. Prism themes use one of each, so normalize
 * their root <pre> rule before a light/dark theme switch reaches the DOM.
 */
function normalizePreBackground(theme: SyntaxTheme): SyntaxTheme {
  const preStyle = theme[PRE_SELECTOR];
  if (!preStyle) return theme;

  const { background, ...rest } = preStyle;
  const normalizedPreStyle = {
    ...rest,
    ...(background === undefined ? {} : { backgroundColor: String(background) }),
  } as CSSProperties;
  return {
    ...theme,
    [PRE_SELECTOR]: normalizedPreStyle,
  };
}

export const lightSyntaxTheme = normalizePreBackground(vs as SyntaxTheme);
export const darkSyntaxTheme = normalizePreBackground(vscDarkPlus as SyntaxTheme);
