/**
 * Engine fetch wrapper.
 *
 * The engine has no authentication (Basic Auth was removed); this wrapper only
 * sets a JSON Content-Type when a body is present and delegates to `fetch`.
 */

/**
 * fetch() with a JSON Content-Type header when a body is supplied.
 */
export async function authFetch(url: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (init.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  return fetch(url, { ...init, headers });
}
