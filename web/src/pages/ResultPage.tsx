import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconSearch } from '../icons'
import type { Book, ExploreEntry } from '../types'
import BookRow from './BookRow'

export default function ResultPage({ explore = false }: { explore?: boolean }) {
  const [params] = useSearchParams()
  const key = params.get('key') || ''
  const origin = params.get('origin') || ''
  const navigate = useNavigate()
  const toast = useToast()
  const [books, setBooks] = useState<Book[]>([])
  const [loading, setLoading] = useState(true)
  const [input, setInput] = useState(key)
  const [page, setPage] = useState(1)
  const [exploreEntries, setExploreEntries] = useState<ExploreEntry[]>([])
  const [activeEntry, setActiveEntry] = useState(params.get('url') || '')

  useEffect(() => {
    if (explore && origin) {
      api.listSources().then(srcs => {
        const src = srcs.find(s => s.bookSourceUrl === origin)
        if (src?.exploreUrl) {
          try { setExploreEntries(JSON.parse(src.exploreUrl as string)) } catch { /* ignore */ }
        }
      }).catch(() => {})
    }
  }, [explore, origin])

  const load = () => {
    setLoading(true)
    const p = explore
      ? api.explore(origin, activeEntry || params.get('url') || '', page)
      : api.search(key, page, origin || undefined)
    p.then(r => setBooks(r.books))
      .catch(e => toast(e.message))
      .finally(() => setLoading(false))
  }
  useEffect(load, [key, origin, page, activeEntry])

  const openBook = (b: Book) => {
    navigate(`/book?origin=${encodeURIComponent(b.origin)}&url=${encodeURIComponent(b.bookUrl)}&name=${encodeURIComponent(b.name)}&author=${encodeURIComponent(b.author)}&intro=${encodeURIComponent(b.intro)}&cover=${encodeURIComponent(b.coverUrl)}&kind=${encodeURIComponent(b.kind)}&wordcount=${encodeURIComponent(b.wordCount)}&lastchapter=${encodeURIComponent(b.lastChapter)}`)
  }

  const title = explore ? '发现' : `搜索: ${key}`

  return (
    <>
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title">{title}</div>
        {!explore && <button className="icon-btn" onClick={() => navigate(`/search?key=${encodeURIComponent(key)}`)}><IconSearch /></button>}
      </header>
      <main className="page">
        {explore && exploreEntries.length > 0 && (
          <div className="chips">
            {exploreEntries.map((e, i) => (
              <button key={i} className={`chip ${activeEntry === e.url || (!activeEntry && i === 0) ? 'active' : ''}`}
                onClick={() => { setActiveEntry(e.url); setPage(1) }}>{e.title}</button>
            ))}
          </div>
        )}
        {!explore && (
          <form className="search-bar" onSubmit={e => { e.preventDefault(); if (input.trim()) navigate(`/result?key=${encodeURIComponent(input.trim())}`) }}>
            <IconSearch />
            <input value={input} onChange={e => setInput(e.target.value)} placeholder="搜索书名、作者" />
          </form>
        )}
        {loading ? <div className="loading"><div className="spinner" />正在搜索所有书源…</div>
          : books.length === 0 ? <div className="empty"><div>没有找到相关书籍</div><p style={{ fontSize: 13, marginTop: 8 }}>试试其他关键词或检查书源</p></div>
          : (
            <>
              <div className="list-section-title">找到 {books.length} 本书</div>
              {books.map((b, i) => <BookRow key={`${b.origin}-${b.bookUrl}-${i}`} book={b} onClick={() => openBook(b)} />)}
              <div style={{ display: 'flex', gap: 10, marginTop: 16 }}>
                <button className="btn btn-ghost" disabled={page <= 1} onClick={() => setPage(p => p - 1)}>上一页</button>
                <button className="btn btn-ghost" style={{ flex: 1 }}>第 {page} 页</button>
                <button className="btn btn-ghost" onClick={() => setPage(p => p + 1)}>下一页</button>
              </div>
            </>
          )}
      </main>
    </>
  )
}
