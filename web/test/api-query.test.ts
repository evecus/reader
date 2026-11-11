import { describe, expect, it } from 'vitest';
import { ReaderApi } from '../src/api/client.ts';
import type { Session } from '../src/api/types.ts';
import { FakeTransport, makePlatform } from './helpers/env.ts';

const session: Session = {
  user: { id: 'reader', username: 'reader', displayName: 'Reader', role: 'member', createdAt: 0 },
  accessToken: 'access', refreshToken: 'refresh', accessTokenExpiresAt: 9e15, refreshTokenExpiresAt: 9e15,
};

async function apiWith(transport: FakeTransport): Promise<ReaderApi> {
  const api = new ReaderApi(makePlatform(transport), { load: async () => session, save: async () => {}, clear: async () => {} });
  await api.restore();
  return api;
}

describe('ReaderApi navigation query cache', () => {
  it('refreshes explicitly and separates pages', async () => {
    const transport = new FakeTransport();
    transport.json({ items: [], total: 0 });
    const api = await apiWith(transport);
    await api.listBooks({ page: 1 });
    await api.listBooks({ page: 2 });
    api.refreshQueries();
    await api.listBooks({ page: 1 });
    expect(transport.requests).toHaveLength(3);
  });

  it('leaves non-navigation reads (notes) uncached', async () => {
    const transport = new FakeTransport();
    transport.json({ notes: [] });
    const api = await apiWith(transport);
    await api.listNotes();
    await api.listNotes();
    expect(transport.countMatching(request => request.url.startsWith('/api/v1/notes'))).toBe(2);
  });

  it('deduplicates concurrent shelf reads and reuses them on return', async () => {
    const transport = new FakeTransport();
    transport.json({ items: [{ id: 'book-1' }], total: 1 });
    const api = await apiWith(transport);
    await Promise.all([api.listBooks(), api.listBooks()]);
    await api.listBooks();
    expect(transport.countMatching(request => request.url.startsWith('/api/v1/books'))).toBe(1);
  });

  it('invalidates navigation data after a write', async () => {
    const transport = new FakeTransport();
    transport.respondWith(request => request.method === 'GET'
      ? { status: 200, headers: {}, json: { items: [{ id: 'book-1' }], total: 1 } }
      : { status: 200, headers: {}, json: {} });
    const api = await apiWith(transport);
    await api.listBooks();
    await api.browseBatchShelf({ bookIds: ['book-1'] }, 'remove');
    await api.listBooks();
    expect(transport.countMatching(request => request.url.startsWith('/api/v1/books'))).toBe(2);
  });
});
