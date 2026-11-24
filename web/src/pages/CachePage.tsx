import { useCallback, useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconDownload, IconTrash, IconClose } from '../icons'
import type { CacheBook, CacheItem, DownloadTask } from '../types'

const statusText: Record<DownloadTask['status'], string> = {
  pending: '等待中', running: '下载中', done: '已完成', cancelled: '已取消', error: '失败',
}

export default function CachePage() {
  const [books, setBooks] = useState<CacheBook[]>([])
  const [tasks, setTasks] = useState<DownloadTask[]>([])
  const [loading, setLoading] = useState(true)
  // 展开章节列表的书
  const [expanded, setExpanded] = useState<string>('')
  const [bookChapters, setBookChapters] = useState<Record<string, CacheItem[]>>({})
  const toast = useToast()
  const navigate = useNavigate()

  const loadBooks = useCallback(() => {
    return api.listCacheBooks().then(setBooks).catch(e => toast(e.message))
  }, [toast])
  const loadTasks = useCallback(() => {
    return api.listDownloads().then(setTasks).catch(() => {})
  }, [])

  useEffect(() => { Promise.all([loadBooks(), loadTasks()]).finally(() => setLoading(false)) }, [])

  // 有活跃任务时轮询进度
  const hasActive = tasks.some(t => t.status === 'pending' || t.status === 'running')
  useEffect(() => {
    if (!hasActive) return
    const timer = setInterval(() => { loadTasks().then(loadBooks) }, 1000)
    return () => clearInterval(timer)
  }, [hasActive, loadTasks, loadBooks])

  const fmtSize = (n: number) => n > 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${(n / 1024).toFixed(1)} KB`
  const pct = (t: DownloadTask) => t.total > 0 ? Math.min(100, Math.round((t.done + t.failed) / t.total * 100)) : 0

  const totalChapters = books.reduce((s, b) => s + b.chapterCount, 0)
  const totalSize = books.reduce((s, b) => s + b.totalSize, 0)
  const activeCount = tasks.filter(t => t.status === 'pending' || t.status === 'running').length
  const historyTasks = tasks.filter(t => !(t.status === 'pending' || t.status === 'running'))
  const hasFinished = historyTasks.length > 0

  const readBook = (b: CacheBook) => {
    const q = new URLSearchParams({
      origin: b.origin, book_url: b.bookUrl,
      name: b.bookName || '未命名书籍', author: b.bookAuthor || '',
      from: 'cache', index: '0',
    })
    navigate(`/reader?${q}`)
  }

  const toggleExpand = async (b: CacheBook) => {
    if (expanded === b.bookUrl) { setExpanded(''); return }
    setExpanded(b.bookUrl)
    if (!bookChapters[b.bookUrl]) {
      try {
        const r = await api.listCacheChapters(b.bookUrl)
        setBookChapters(m => ({ ...m, [b.bookUrl]: r.chapters }))
      } catch (e) { toast((e as Error).message) }
    }
  }

  const removeBook = async (b: CacheBook) => {
    if (!confirm(`确定删除《${b.bookName || '未命名书籍'}》的全部 ${b.chapterCount} 章缓存？`)) return
    try {
      await api.deleteCacheBook(b.bookUrl)
      toast('已删除该书缓存')
      if (expanded === b.bookUrl) setExpanded('')
      loadBooks()
    } catch (e) { toast((e as Error).message) }
  }

  const removeChapter = async (c: CacheItem) => {
    try { await api.deleteCache(c.bookUrl, c.chapterUrl); toast('已删除该章缓存') } catch (e) { toast((e as Error).message) }
    // 刷新分组与已展开的章节列表
    await loadBooks()
    const r = await api.listCacheChapters(c.bookUrl).catch(() => null)
    if (r) setBookChapters(m => ({ ...m, [c.bookUrl]: r.chapters }))
  }

  const clearAll = async () => {
    if (!confirm('确定清空所有离线缓存？')) return
    try { await api.clearCache(); toast('已清空'); setExpanded(''); loadBooks() }
    catch (e) { toast((e as Error).message) }
  }

  const cancel = async (t: DownloadTask) => {
    try { await api.cancelDownload(t.id); toast('已取消下载'); loadTasks() }
    catch (e) { toast((e as Error).message) }
  }

  const clearHistory = async () => {
    try { await api.clearFinishedDownloads(); toast('已清除历史记录'); loadTasks() }
    catch (e) { toast((e as Error).message) }
  }

  const bookDisplayName = (b: CacheBook) => b.bookName || '未命名书籍（旧缓存）'

  return (
    <>
      <PageHeader
        title="离线缓存"
        subtitle="管理后台下载任务与已缓存的书籍"
        backTo="/shelf"
        actions={<>
          {hasFinished && <button className="btn btn-ghost" onClick={clearHistory}><IconClose /> 清除记录</button>}
          {books.length > 0 && <button className="btn btn-ghost" onClick={clearAll}><IconTrash /> 清空缓存</button>}
        </>}
      />

      <div className="stat-row">
        <div className="stat-card"><div className="stat-label">已缓存书籍</div><div className="stat-value">{books.length}</div></div>
        <div className="stat-card"><div className="stat-label">缓存章节</div><div className="stat-value">{totalChapters}</div></div>
        <div className="stat-card"><div className="stat-label">缓存占用</div><div className="stat-value">{fmtSize(totalSize)}</div></div>
        <div className="stat-card"><div className="stat-label">进行中任务</div><div className="stat-value">{activeCount}</div></div>
      </div>

      {tasks.length > 0 && (
        <>
          <div className="section-title">下载任务 <span className="count">({tasks.length})</span></div>
          <div className="stack" style={{ marginBottom: 22 }}>
            {tasks.map(t => (
              <div className="download-task" key={t.id}>
                <div className="dtop">
                  <IconDownload style={{ width: 18, height: 18, color: 'var(--primary)', flexShrink: 0 }} />
                  <div className="dt-name">{t.bookName || '未命名书籍'}</div>
                  <span className={`dt-status st-${t.status}`}>{statusText[t.status]}</span>
                  {(t.status === 'pending' || t.status === 'running') &&
                    <button className="icon-btn-sm" onClick={() => cancel(t)} title="取消"><IconClose /></button>}
                </div>
                {(t.status === 'running' || t.status === 'done' || t.status === 'cancelled') && (
                  <div className="dt-progress">
                    <div className="dt-bar"><div className="dt-bar-fill" style={{ width: `${pct(t)}%` }} /></div>
                    <div className="dt-meta">{t.done}/{t.total || '?'} 章{pct(t) > 0 ? ` · ${pct(t)}%` : ''}{t.failed > 0 ? ` · 失败 ${t.failed}` : ''}</div>
                  </div>
                )}
                {t.message && <div className="dt-message">{t.message}</div>}
              </div>
            ))}
          </div>
        </>
      )}

      <div className="section-title">已缓存书籍 <span className="count">({books.length})</span></div>
      {loading ? <div className="loading"><div className="spinner" />加载中…</div>
        : books.length === 0 ? (
          <div className="empty">
            <IconDownload style={{ width: 54, height: 54, opacity: .3, margin: '0 auto 14px' }} />
            <div className="empty-title">暂无离线缓存</div>
            <p>在书籍信息页点击「下载全书」批量缓存，或在阅读页缓存单章</p>
          </div>
        ) : (
          <div className="stack">
            {books.map(b => (
              <div className="cache-book" key={b.bookUrl}>
                <div className="cache-book-main" onClick={() => readBook(b)} title="点击阅读">
                  <div className="cb-cover">{(b.bookName || '书').slice(0, 2)}</div>
                  <div className="cb-info">
                    <div className="cb-name">{bookDisplayName(b)}</div>
                    <div className="cb-meta">
                      {b.bookAuthor && <span>{b.bookAuthor} · </span>}
                      {b.chapterCount} 章 · {fmtSize(b.totalSize)} · {new Date(b.lastCachedAt).toLocaleString('zh-CN')}
                    </div>
                  </div>
                  <button className="btn btn-soft cb-read" onClick={e => { e.stopPropagation(); readBook(b) }}>阅读</button>
                  <button className="icon-btn-sm" onClick={e => { e.stopPropagation(); toggleExpand(b) }}
                    title={expanded === b.bookUrl ? '收起章节' : '查看章节'}>
                    {expanded === b.bookUrl ? <IconClose /> : <IconListIcon />}
                  </button>
                  <button className="icon-btn-sm" onClick={e => { e.stopPropagation(); removeBook(b) }} title="删除整本书"><IconTrash /></button>
                </div>
                {expanded === b.bookUrl && (
                  <div className="cache-book-chapters">
                    {(bookChapters[b.bookUrl] || []).length === 0
                      ? <div className="cb-empty">加载中…</div>
                      : bookChapters[b.bookUrl].map(c => (
                        <div className="cb-chapter" key={c.chapterUrl}>
                          <span className="ct-title">{c.title || c.chapterUrl}</span>
                          <span className="cb-ch-size">{fmtSize(c.size)}</span>
                          <button className="icon-btn-sm" onClick={() => removeChapter(c)} title="删除本章"><IconTrash /></button>
                        </div>
                      ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
    </>
  )
}

function IconListIcon() {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="17" height="17">
    <path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01" strokeLinecap="round" />
  </svg>
}
