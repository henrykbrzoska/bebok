/**
 * Relative-link resolution for the Preview panel (F6-10).
 *
 * Engine filesystem paths (`FsEntry.path`, `FsFileResponse.path`) come back in
 * the host separator (`\` on Windows, `/` on Unix - see `explorer.ts`'s own
 * "Engine paths use the host separator" comment). Markdown links, by
 * convention, always use `/`. Per the cross-platform requirement, this must
 * not assume either separator when joining a link against a `basePath`: we
 * split the base on *both* `\` and `/` to get its directory segments, split
 * the (always-`/`) link on `/`, resolve `.`/`..`, and rejoin with `/` - which
 * `Path`/`PathBuf` on Windows accepts as a separator too, so the joined path
 * stays valid for the engine's `/fs/file` endpoint on every OS.
 */

/** True when `href` carries an explicit URI scheme (`http:`, `mailto:`, ...). */
export function hasUriScheme(href: string): boolean {
  return /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(href);
}

/**
 * Resolve a markdown link's `href` against the engine path of the document it
 * appears in. Returns `null` for anything that is not a plain relative path
 * (absolute URLs, `mailto:`, a leading `/`, or a Windows drive letter) - the
 * caller should leave those as ordinary, non-intercepted links.
 */
export function resolveRelativePath(basePath: string | null, href: string): string | null {
  const clean = href.split(/[?#]/)[0]?.trim();
  if (!clean || hasUriScheme(clean) || clean.startsWith('/') || /^[a-zA-Z]:[\\/]/.test(clean)) {
    return null;
  }

  const baseDir = (basePath ?? '').split(/[\\/]+/).filter((s) => s.length > 0);
  baseDir.pop(); // drop the file name, keep the containing directory segments

  const segments = [...baseDir];
  for (const part of clean.split('/')) {
    if (part === '' || part === '.') {
      continue;
    }
    if (part === '..') {
      segments.pop();
      continue;
    }
    segments.push(part);
  }

  return segments.join('/');
}
