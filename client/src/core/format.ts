/** Small formatting helpers for rendering tool parts / permission inputs. */

/** Pretty-print a JSON value (unknown inputs from engine tool calls). */
export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** A short single-line summary of a tool call's arguments (for the badge). */
export function summarizeInput(value: unknown, max = 80): string {
  if (typeof value === 'string') {
    return value.length > max ? `${value.slice(0, max)}…` : value;
  }
  if (value && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    const parts: string[] = [];
    for (const [key, val] of Object.entries(record)) {
      if (val === undefined) {
        continue;
      }
      const text = typeof val === 'string' ? val : JSON.stringify(val);
      parts.push(`${key}=${text.length > 40 ? `${text.slice(0, 40)}…` : text}`);
    }
    return parts.join(' ');
  }
  const text = JSON.stringify(value);
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

export function formatMs(ms: number): string {
  return new Date(ms).toLocaleTimeString();
}
