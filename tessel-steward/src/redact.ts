const TOKEN_PATTERN = /art_v[\w.~+/=-]*/g;

/** Replaces anything shaped like an Artifacts token (`art_v…`) so output can be shown or logged. */
export function redactTokens(text: string): string {
  return text.replace(TOKEN_PATTERN, "art_v…[redacted]");
}
