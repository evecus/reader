import type { Book, BookChapter, BookSource, CacheItem, ShelfItem, DownloadTask } from './types'

async function req<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(path, options)
  if (!res.ok) {
    let msg = `请求失败 (${res.status})`
    try {
      const data = await res.json()
      msg = data.error?.message || msg
    } catch { /* ignore */ }
    throw new Error(msg)
  }
  if (res.status === 204) return undefined as T
  return res.json()
}

const json = (body: unknown): RequestInit => ({
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify(body),
})

export const api = {
  health: () => req<{ status: string }>('/api/health'),

  listSources: () => req<BookSource[]>('/api/sources'),
  importSources: (content: string) => req<{ imported: number }>('/api/sources', {
    method: 'POST', ...json({ content }),
  }),
  deleteSource: (url: string) => req<void>(`/api/sources/${encodeURIComponent(url)}`, { method: 'DELETE' }),
  toggleSource: (url: string, enabled: boolean) => req<void>(`/api/sources/${encodeURIComponent(url)}/toggle`, {
    method: 'PATCH', ...json({ enabled }),
  }),
  testSource: (url: string) => req<{ ok: boolean; count?: number; error?: string }>('/api/sources/test', {
    method: 'POST', ...json({ url }),
  }),

  search: (key: string, page = 1, origin?: string) => {
    const q = new URLSearchParams({ key, page: String(page) })
    if (origin) q.set('origin', origin)
    return req<{ books: Book[]; errors: { source: string; error: string }[] }>(`/api/search?${q}`)
  },
  explore: (origin: string, url: string, page = 1) =>
    req<{ books: Book[]; errors?: { source: string; error: string }[] }>(`/api/explore?${new URLSearchParams({ origin, url, page: String(page) })}`),

  bookInfo: (origin: string, url: string) =>
    req<Book>(`/api/book/info?${new URLSearchParams({ origin, url })}`),
  chapters: (origin: string, url: string, tocUrl?: string, name?: string, author?: string) => {
    const q = new URLSearchParams({ origin, url })
    if (tocUrl) q.set('toc_url', tocUrl)
    if (name) q.set('name', name)
    if (author) q.set('author', author)
    return req<{ chapters: BookChapter[] }>(`/api/book/chapters?${q}`)
  },
  content: (origin: string, url: string, bookUrl: string, title?: string) => {
    const q = new URLSearchParams({ origin, url, book_url: bookUrl })
    if (title) q.set('title', title)
    return req<{ content: string }>(`/api/book/content?${q}`)
  },

  listShelf: () => req<ShelfItem[]>('/api/shelf'),
  addShelf: (item: ShelfItem) => req<{ ok: boolean }>('/api/shelf', { method: 'POST', ...json(item) }),
  removeShelf: (bookUrl: string, origin: string) =>
    req<void>(`/api/shelf/${encodeURIComponent(bookUrl)}?origin=${encodeURIComponent(origin)}`, { method: 'DELETE' }),
  updateProgress: (bookUrl: string, origin: string, chapterIndex: number, chapterTitle: string) =>
    req<{ ok: boolean }>('/api/shelf/progress', {
      method: 'PATCH', ...json({ book_url: bookUrl, origin, chapter_index: chapterIndex, chapter_title: chapterTitle }),
    }),

  listCache: () => req<CacheItem[]>('/api/cache'),
  cacheChapter: (data: { book_url: string; chapter_url: string; title: string; content: string }) =>
    req<{ ok: boolean }>('/api/cache/chapter', { method: 'POST', ...json(data) }),
  deleteCache: (bookUrl: string, chapterUrl: string) =>
    req<void>(`/api/cache/chapter?${new URLSearchParams({ book_url: bookUrl, chapter_url: chapterUrl })}`, { method: 'DELETE' }),
  clearCache: () => req<void>('/api/cache', { method: 'DELETE' }),

  // 整本书后台下载
  startDownload: (data: { origin: string; book_url: string; toc_url: string; name: string; author: string }) =>
    req<DownloadTask>('/api/download', { method: 'POST', ...json(data) }),
  listDownloads: () => req<DownloadTask[]>('/api/download'),
  getDownload: (id: string) => req<DownloadTask>(`/api/download/${encodeURIComponent(id)}`),
  cancelDownload: (id: string) => req<void>(`/api/download/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  clearFinishedDownloads: () => req<{ cleared: number }>('/api/download', { method: 'DELETE' }),
}
