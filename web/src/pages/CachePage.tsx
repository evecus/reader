import { useCallback, useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconDownload, IconTrash, IconClose } from '../icons'
import type { CacheItem, DownloadTask } from '../types'

const statusText: Record<DownloadTask['status'], string> = {
  pending: '等待中', running: '下载中', done: '已完成', cancelled: '已取消', error: '失败',
}

export default function CachePage() {
  const [items, setItems] = useState<CacheItem[]>([])
  const [tasks, setTasks] = useState<DownloadTask[]>([])
  const [loading, setLoading] = useState(true)
  const navigate = useNavigate()
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
  const fmtSize = (n: number) => n > 1024 * 1024 ? `${(n / 1024 / 1024).toFixed(1)}MB` : `${(n / 1024).toFixed(1)}KB`
  const pct = (t: DownloadTask) => t.total > 0 ? Math.min(100, Math.round((t.done + t.failed) / t.total * 100)) : 0

  const activeTasks = tasks.filter(t => t.status === 'pending' || t.status === 'running')
  const historyTasks = tasks.filter(t => !(t.status === 'pending' || t.status === 'running'))
  const hasFinished = historyTasks.length > 0

  return (
    <>
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate('/shelf')}><IconBack /></button>
        <div className="title">离线缓存</div>
        {hasFinished && <button className="icon-btn" onClick={clearHistory} title="清除下载记录"><IconClose /></button>}
        {items.length > 0 && <button className="icon-btn" onClick={clearAll} title="清空缓存"><IconTrash /></button>}
      </header>
      <main className="page">
        {/* 下载任务区 */}
        {tasks.length > 0 && (
          <div className="list-section-title">下载任务</div>
        )}
        {tasks.map(t => (
          <div className="download-task" key={t.id}>
            <div className="dtop">
              <IconDownload style={{ width: 18, height: 18, color: 'var(--primary)' }} />
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
            <div className="dt-message">{t.message}</div>
          </div>
        ))}

        {/* 已缓存章节 */}
        <div className="list-section-title" style={{ marginTop: 12 }}>已缓存 {items.length} 章 · 共 {fmtSize(totalSize)}</div>
        {loading ? <div className="loading"><div className="spinner" />加载中…</div>
          : items.length === 0 ? (
            <div className="empty">
              <IconDownload style={{ width: 56, height: 56, opacity: .3 }} />
              <div>暂无离线缓存</div>
              <p style={{ fontSize: 13, marginTop: 8 }}>在书籍信息页点击"下载全书"批量缓存，或在阅读页缓存单章</p>
            </div>
          ) : items.map((c, i) => (
            <div className="cache-item" key={i}>
              <IconDownload style={{ width: 18, height: 18, color: 'var(--primary)' }} />
              <div className="info">
                <div className="title">{c.title || c.chapterUrl}</div>
                <div className="meta">{fmtSize(c.size)} · {new Date(c.cachedAt).toLocaleString('zh-CN')}</div>
              </div>
              <button className="icon-btn-sm" onClick={() => remove(c)}><IconTrash /></button>
            </div>
          ))}
      </main>
    </>
  )
}
