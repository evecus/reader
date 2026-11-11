import { QueryClient } from '@tanstack/query-core';

/**
 * Shared client-side server cache for transport-level invalidation. React pages
 * consume the same client through QueryClientProvider and React Query hooks;
 * this module only owns the API client's framework-neutral cache operations.
 */
export const createReaderQueryClient = () => new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5 * 60_000,
      gcTime: 10 * 60_000,
      // ReaderApi owns auth refresh and transport retry semantics. Query retry
      // would retry 401s and account-change aborts with the new credentials.
      retry: false,
      refetchOnWindowFocus: false,
    },
  },
});

export const readerQueryKey = (baseUrl: string, userId: string | undefined, scope: number, path: string) =>
  ['reader', baseUrl, userId ?? 'anonymous', scope, path] as const;

/** Only navigation data is cached; auth, sync and jobs stay live. */
export function isNavigationQuery(path: string): boolean {
  const resource = path.split('?')[0]!;
  return /^\/api\/v1\/(books|books\/[^/]+|sources|sources\/types|plugins|subscriptions|library\/(browse|facets|continue))$/.test(resource)
    || /^\/api\/v1\/sources\/[^/]+\/(browse|entries|search-filters)$/.test(resource);
}

/** Invalidate only the navigation resources a mutation can change. */
export function affectedNavigationQuery(mutation: string, query: string): boolean {
  if (/^\/api\/v1\/sync(?:\/|$)/.test(mutation)) return query.startsWith('/api/v1/library/continue');
  if (/^\/api\/v1\/(notes|tts|ai|auth)(?:\/|$)/.test(mutation)) return false;
  if (mutation === '/api/v1/library/upload') return query.startsWith('/api/v1/books') || query.startsWith('/api/v1/library/browse');
  if (mutation === '/api/v1/library/browse/shelf') return query.startsWith('/api/v1/books') || query.startsWith('/api/v1/library/continue');
  if (/^\/api\/v1\/library\/browse\//.test(mutation)) return query.startsWith('/api/v1/library/browse') || query.startsWith('/api/v1/books');
  if (/^\/api\/v1\/books\//.test(mutation)) {
    const id = mutation.match(/^\/api\/v1\/books\/([^/]+)/)?.[1];
    return query.startsWith('/api/v1/books') && (!id || query.includes('/books/' + id));
  }
  if (/^\/api\/v1\/sources(?:\/|$)/.test(mutation) || /^\/api\/v1\/plugins/.test(mutation)) return query.startsWith('/api/v1/sources') || query.startsWith('/api/v1/plugins') || query.startsWith('/api/v1/subscriptions');
  return false;
}
