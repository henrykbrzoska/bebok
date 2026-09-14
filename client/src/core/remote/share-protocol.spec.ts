import { ShareUrlError, parseShareUrl, shareTargetId } from './share-protocol';

describe('parseShareUrl (1.8)', () => {
  const sid = '11111111-2222-3333-4444-555555555555';
  const link = `bebok://share?v=1&ep=http%3A%2F%2F100.64.0.7%3A8790%2Chttps%3A%2F%2Fr.workers.dev%2Ft%2F${'a'.repeat(32)}&s=${sid}&t=${'t'.repeat(40)}&n=rafal-pc&fp=39f7cb9b`;

  it('parses endpoints, session, token and engine name', () => {
    const invite = parseShareUrl(`  ${link}\n`);
    expect(invite.version).toBe(1);
    expect(invite.endpoints).toEqual([
      'http://100.64.0.7:8790',
      `https://r.workers.dev/t/${'a'.repeat(32)}`,
    ]);
    expect(invite.sessionId).toBe(sid);
    expect(invite.token).toBe('t'.repeat(40));
    expect(invite.engineName).toBe('rafal-pc');
    expect(invite.fingerprint).toBe('39f7cb9b');
    expect(shareTargetId(invite.sessionId)).toBe(`share:${sid}`);
  });

  it('rejects other links, versions, missing fields and public http endpoints', () => {
    expect(() => parseShareUrl('https://example.com')).toThrowMatching(
      (e) => e instanceof ShareUrlError && e.reason === 'not_share_url',
    );
    expect(() => parseShareUrl(`bebok://pair?v=1&ep=x&code=ABCDEFGH`)).toThrowMatching(
      (e) => e instanceof ShareUrlError && e.reason === 'not_share_url',
    );
    expect(() => parseShareUrl(link.replace('v=1', 'v=2'))).toThrowMatching(
      (e) => e instanceof ShareUrlError && e.reason === 'unsupported_version',
    );
    expect(() => parseShareUrl(link.replace(`&s=${sid}`, '&s=nope'))).toThrowMatching(
      (e) => e instanceof ShareUrlError && e.reason === 'missing_field',
    );
    expect(() =>
      parseShareUrl(
        `bebok://share?v=1&ep=http%3A%2F%2F8.8.8.8%3A8790&s=${sid}&t=${'t'.repeat(40)}`,
      ),
    ).toThrowMatching((e) => e instanceof ShareUrlError && e.reason === 'no_endpoints');
  });
});
