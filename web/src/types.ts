export interface Book {
  bookUrl: string
  origin: string
  originName: string
  name: string
  author: string
  kind: string
  wordCount: string
  lastChapter: string
  intro: string
  coverUrl: string
  tocUrl: string
  origins: string[]
}

export interface BookChapter {
  index: number
  title: string
  url: string
  isVolume: boolean
  isVip: boolean
}

export interface ShelfItem {
  id: string
  book: Book
  durChapterIndex: number
  durChapterTitle: string
  addedAt: number
  updatedAt: number
}

export interface BookSource {
  bookSourceUrl: string
  bookSourceName: string
  bookSourceGroup: string
  enabled: boolean
  customOrder: number
  lastUpdateTime: number
  respondTime: number
  searchUrl?: string
  exploreUrl?: string
  bookSourceComment?: string
  [key: string]: unknown
}

export interface ExploreEntry {
  title: string
  url: string
  style?: Record<string, unknown>
}

export interface CacheItem {
  bookUrl: string
  chapterUrl: string
  title: string
  cachedAt: number
  size: number
}

export interface DownloadTask {
  id: string
  bookUrl: string
  origin: string
  bookName: string
  total: number
  done: number
  failed: number
  status: 'pending' | 'running' | 'done' | 'cancelled' | 'error'
  message: string
  createdAt: number
  finishedAt?: number
}
