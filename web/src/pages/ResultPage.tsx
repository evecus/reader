import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconSearch, IconSettings } from '../icons'
import type { Book, ExploreEntry } from '../types'
import BookRow from './BookRow'

export default function ResultPage({ explore = false }: { explore?: boolean }) {
  const [params] = useSearchParams()
  const key = params.get('key') || ''
  const origin = params.get('origin') || ''
  const navigate = useNavigate()
  const toast = useToast()
  const [books, setBooks] = useState<Book[]>([])
  const [errors, setErrors] = useState<{ source: string; error: string }[]>([])
  const [loading, setLoading] = useState(true)
  const [noSource, setNoSource] = useState(false)
  const [input, setInput] = useState(key)
  const [page, setPage] = useState(1)
  const [exploreEntries, setExploreEntries] = useState<ExploreEntry[]>([])
  const [activeEntry, setActiveEntry] = useState(params.get('url') || '')

  // 底部「发现」入口没有带 origin：自动选第一个启用书源
  useEffect(() => {
    if (!explore || origin) return
    api.listSources().then(srcs => {
      const src = srcs.find(s => s.enabled) || srcs[0]
      if (!src) { setNoSource(true); setLoading(false); return }
      let firstUrl = ''
      try {
        const entries: ExploreEntry[] = JSON.parse(src.exploreUrl || '[]')
        firstUrl = entries.find(e => e.url)?.url || ''
      } catch { /* ignore */ }
      const q = new URLSearchParams({ origin: src.bookSourceUrl })
      if (firstUrl) q.set('url', firstUrl)
      navigate(`/explore?${q}`, { replace: true })
    }).catch(e => { toast(e.message); setLoading(false) })
  }, [explore, origin])

  // 加载书源的发现分类，未指定分类时默认选中第一个有效分类
  useEffect(() => {
    if (!explore || !origin) return
    api.listSources().then(srcs => {
      const src = srcs.find(s => s.bookSourceUrl === origin)
      if (src?.exploreUrl) {
        try {
          const entries: ExploreEntry[] = JSON.parse(src.exploreUrl as string)
          setExploreEntries(entries)
          if (!activeEntry) {
            const first = entries.find(e => e.url)
            if (first) setActiveEntry(first.url)
          }
        } catch { /* ignore */ }
      }
    }).catch(() => {})
  }, [explore, origin])

  const load = () => {
    if (explore && !origin) return
    if (explore && exploreEntries.length > 0 && !activeEntry) return
    setLoading(true)
    const p = explore
      ? api.explore(origin, activeEntry || params.get('url') || '', page)
      : api.search(key, page, origin || undefined)
    p.then(r => { setBooks(r.books); setErrors(r.errors || []) })
      .catch(e => { setErrors([]); toast(e.message) })
      .finally(() => setLoading(false))
  }
  useEffect(load, [key, origin, page, activeEntry, explore, exploreEntries])

  const openBook = (b: Book) => {
    navigate(`/book?origin=${encodeURIComponent(b.origin)}&url=${encodeURIComponent(b.bookUrl)}&name=${encodeURIComponent(b.name)}&author=${encodeURIComponent(b.author)}&intro=${encodeURIComponent(b.intro)}&cover=${encodeURIComponent(b.coverUrl)}&kind=${encodeURIComponent(b.kind)}&wordcount=${encodeURIComponent(b.wordCount)}&lastchapter=${encodeURIComponent(b.lastChapter)}`)
  }

  const title = explore ? '发现' : `搜索「${key}」`

  return (
    <>
      <PageHeader
        title={title}
        subtitle={explore ? '浏览书源提供的分类榜单' : '来自全部已启用书源的搜索结果'}
        backTo={-1}
        actions={explore
          ? <button className="btn btn-ghost" onClick={() => navigate('/sources')}><IconSettings /> 管理书源</button>
          : <button className="btn btn-ghost" onClick={() => navigate(`/search?key=${encodeURIComponent(key)}`)}><IconSearch /> 修改搜索</button>}
      />

      {explore && exploreEntries.length > 0 && (
        <div className="chips">
          {exploreEntries.map((e, i) => (
            <button key={i} className={`chip ${activeEntry === e.url || (!activeEntry && i === 0) ? 'active' : ''}`}
              onClick={() => { setActiveEntry(e.url); setPage(1) }}>{e.title}</button>
          ))}
        </div>
      )}
      {!explore && (
        <form className="search-bar" style={{ marginBottom: 18 }} onSubmit={e => { e.preventDefault(); if (input.trim()) navigate(`/result?key=${encodeURIComponent(input.trim())}`) }}>
          <IconSearch />
          <input value={input} onChange={e => setInput(e.target.value)} placeholder="搜索书名、作者" />
          <button className="btn btn-primary" type="submit">搜索</button>
        </form>
      )}

      {errors.length > 0 && (
        <div className="search-errors">
          {errors.map((e, i) => <div key={i}>{e.source}：{e.error}</div>)}
        </div>
      )}

      {loading ? <div className="loading"><div className="spinner" />{explore ? '正在加载…' : '正在搜索所有书源…'}</div>
        : noSource ? (
          <div className="empty">
            <div className="empty-title">还没有书源</div>
            <p>请先导入书源再使用发现功能</p>
            <button className="btn btn-primary" onClick={() => navigate('/sources')}>去导入书源</button>
          </div>
        ) : books.length === 0 ? (
          <div className="empty">
            <div className="empty-title">没有找到相关书籍</div>
            <p>{errors.length > 0 ? '书源请求失败，请检查上方错误信息' : '试试其他关键词或检查书源'}</p>
            {explore && <button className="btn btn-ghost" onClick={() => navigate('/sources')}>管理书源</button>}
          </div>
        ) : (
          <>
            <div className="section-title">找到 {books.length} 本书 <span className="count">· 第 {page} 页</span></div>
            <div className="book-grid">
              {books.map((b, i) => <BookRow key={`${b.origin}-${b.bookUrl}-${i}`} book={b} onClick={() => openBook(b)} />)}
            </div>
            <div style={{ display: 'flex', justifyContent: 'center', gap: 10, marginTop: 24 }}>
              <button className="btn btn-ghost" disabled={page <= 1} onClick={() => setPage(p => p - 1)}>上一页</button>
              <button className="btn btn-ghost" onClick={() => setPage(p => p + 1)}>下一页</button>
            </div>
          </>
        )}
    </>
  )
}
