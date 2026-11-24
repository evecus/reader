import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconDownload, IconTrash, IconClose } from '../icons'
import type { CacheItem, DownloadTask } from '../types'

const statusText: Record<DownloadTask['status'], string> = {
  pending: '等待中', running: '下载中', done: '已完成', cancelled: '已取消', error: '失败',
}

export default function CachePage() {
  const [items, setItems] = useState<CacheItem[]>([])
  const [tasks, setTasks] = useState<DownloadTask[]>([])
  const [loading, setLoading] = useState(true)
  const toast = useToast()

  const loadCache = useCallback(() => {
    return api.listCache().then(setItems).catch(e => toast(e.message))
  }, [toast])
  const loadTasks = useCallback(() => {
    return api.listDownloads().then(setTasks).catch(() => {})
  }, [])

  useEffect(() => { Promise.all([loadCache(), loadTasks()]).finally(() => setLoading(false)) }, [])

  // 有活跃任务时轮询进度
  const hasActive = tasks.some(t => t.status === 'pending' || t.status === 'running')
  useEffect(() => {
    if (!hasActive) return
    const timer = setInterval(() => {
      loadTasks().then(loadCache)
    }, 1000)
    return () => clearInterval(timer)
  }, [hasActive, loadTasks, loadCache])

  const remove = async (c: CacheItem) => {
    try { await api.deleteCache(c.bookUrl, c.chapterUrl); toast('已删除缓存'); loadCache() }
    catch (e) { toast((e as Error).message) }
  }

  const clearAll = async () => {
    if (!confirm('确定清空所有离线缓存？')) return
    try { await api.clearCache(); toast('已清空'); loadCache() }
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

  const totalSize = items.reduce((s, i) => s + i.size, 0)
  const fmtSize = (n: number) => n > 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)} MB` : `${(n / 1024).toFixed(1)} KB`
  const pct = (t: DownloadTask) => t.total > 0 ? Math.min(100, Math.round((t.done + t.failed) / t.total * 100)) : 0

  const activeCount = tasks.filter(t => t.status === 'pending' || t.status === 'running').length
  const historyTasks = tasks.filter(t => !(t.status === 'pending' || t.status === 'running'))
  const hasFinished = historyTasks.length > 0

  return (
    <>
      <PageHeader
        title="离线缓存"
        subtitle="管理后台下载任务与已缓存章节"
        backTo="/shelf"
        actions={<>
          {hasFinished && <button className="btn btn-ghost" onClick={clearHistory}><IconClose /> 清除记录</button>}
          {items.length > 0 && <button className="btn btn-ghost" onClick={clearAll}><IconTrash /> 清空缓存</button>}
        </>}
      />

      <div className="stat-row">
        <div className="stat-card"><div className="stat-label">已缓存章节</div><div className="stat-value">{items.length}</div></div>
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

      <div className="section-title">已缓存章节 <span className="count">({items.length})</span></div>
      {loading ? <div className="loading"><div className="spinner" />加载中…</div>
        : items.length === 0 ? (
          <div className="empty">
            <IconDownload style={{ width: 54, height: 54, opacity: .3, margin: '0 auto 14px' }} />
            <div className="empty-title">暂无离线缓存</div>
            <p>在书籍信息页点击「下载全书」批量缓存，或在阅读页缓存单章</p>
          </div>
        ) : (
          <div className="stack">
            {items.map((c, i) => (
              <div className="cache-item" key={i}>
                <IconDownload />
                <div className="info">
                  <div className="title">{c.title || c.chapterUrl}</div>
                  <div className="meta">{fmtSize(c.size)} · {new Date(c.cachedAt).toLocaleString('zh-CN')}</div>
                </div>
                <button className="icon-btn-sm" onClick={() => remove(c)} title="删除"><IconTrash /></button>
              </div>
            ))}
          </div>
        )}
    </>
  )
}
