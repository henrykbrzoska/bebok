/**
 * F8-4: locale fallback for the "What is Bebok?" page's markdown fetch.
 * `loadAboutMarkdown` takes an injectable `fetchImpl` precisely so this can
 * be pinned without a real network call or TestBed - see `about.ts`.
 */

import { loadAboutMarkdown } from './about';

function fakeFetch(available: Record<string, string>): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    const path = String(input);
    const body = available[path];
    return {
      ok: body !== undefined,
      text: async () => body ?? '',
    } as Response;
  }) as typeof fetch;
}

describe('loadAboutMarkdown', () => {
  it('fetches the localized file when it exists', async () => {
    const fetchImpl = fakeFetch({ 'about/bebok.pl.md': '# Czym jest Bebok?' });
    const text = await loadAboutMarkdown('pl', fetchImpl);
    expect(text).toBe('# Czym jest Bebok?');
  });

  it('falls back to English when the localized file is missing (404)', async () => {
    const fetchImpl = fakeFetch({ 'about/bebok.en.md': '# What is Bebok?' });
    // 'xx' has no translation on disk - only the English file resolves.
    const text = await loadAboutMarkdown('xx', fetchImpl);
    expect(text).toBe('# What is Bebok?');
  });

  it('falls back to English when the localized fetch throws (network error)', async () => {
    const fetchImpl = (async () => {
      throw new Error('network down');
    }) as unknown as typeof fetch;
    // Every request throws, including the English fallback - still rejects.
    await expectAsync(loadAboutMarkdown('pl', fetchImpl)).toBeRejected();
  });

  it('rejects when neither the localized file nor the English fallback exist', async () => {
    const fetchImpl = fakeFetch({});
    await expectAsync(loadAboutMarkdown('pl', fetchImpl)).toBeRejectedWithError(
      /could not be loaded/,
    );
  });

  it('does not double-fetch English when the requested language is already "en"', async () => {
    const calls: string[] = [];
    const fetchImpl = (async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return { ok: false, text: async () => '' } as Response;
    }) as typeof fetch;
    await expectAsync(loadAboutMarkdown('en', fetchImpl)).toBeRejected();
    expect(calls).toEqual(['about/bebok.en.md']);
  });
});
