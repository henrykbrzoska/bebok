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

/**
 * One-line inline preview of a tool call's key argument, for the collapsed
 * row (F6-1b). Picks the argument that best identifies the call: the shell
 * command for `bash`, the target path for the file tools, the URL for
 * `fetch`; any other tool falls back to its first string argument, and
 * failing that the generic `key=value` summary.
 */
export function toolCallPreview(name: string, input: unknown, max = 100): string {
  const truncate = (text: string): string => (text.length > max ? `${text.slice(0, max)}…` : text);
  const record = input && typeof input === 'object' ? (input as Record<string, unknown>) : undefined;

  const field = (...keys: string[]): string | undefined => {
    if (!record) {
      return undefined;
    }
    for (const key of keys) {
      const value = record[key];
      if (typeof value === 'string' && value.trim()) {
        return value;
      }
    }
    return undefined;
  };

  switch (name) {
    case 'bash': {
      const command = field('command', 'cmd');
      if (command) {
        return truncate(command);
      }
      break;
    }
    case 'read_file':
    case 'write_file':
    case 'edit_file':
    case 'append_file': {
      const path = field('path', 'file', 'file_path');
      if (path) {
        return truncate(path);
      }
      break;
    }
    case 'fetch': {
      const url = field('url');
      if (url) {
        return truncate(url);
      }
      break;
    }
  }

  if (record) {
    for (const value of Object.values(record)) {
      if (typeof value === 'string' && value.trim()) {
        return truncate(value);
      }
    }
  }
  return truncate(summarizeInput(input, max));
}

/** Human-readable byte size, e.g. `412 B` / `1.2 kB` / `3.4 MB`. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '';
  }
  if (bytes < 1000) {
    return `${bytes} B`;
  }
  const units = ['kB', 'MB', 'GB'];
  let value = bytes / 1000;
  let unitIndex = 0;
  while (value >= 1000 && unitIndex < units.length - 1) {
    value /= 1000;
    unitIndex += 1;
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`;
}
