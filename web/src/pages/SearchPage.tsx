import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { IconBack, IconSearch } from '../icons'

const HISTORY_KEY = 'legado_search_history'

export default function SearchPage() {
  const [key, setKey] = useState('')
  const [history, setHistory] = useState<string[]>(() => {
    try { return JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]') } catch { return [] }
  })
  const navigate = useNavigate()

  const doSearch = (k: string) => {
    const q = k.trim()
    if (!q) return
    const next = [q, ...history.filter(h => h !== q)].slice(0, 10)
    setHistory(next)
    localStorage.setItem(HISTORY_KEY, JSON.stringify(next))
    navigate(`/result?key=${encodeURIComponent(q)}`)
  }

  return (
    <>
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title" style={{ paddingLeft: 0 }}>搜索</div>
      </header>
      <main className="page">
        <form className="search-bar" onSubmit={e => { e.preventDefault(); doSearch(key) }}>
          <IconSearch />
          <input autoFocus value={key} onChange={e => setKey(e.target.value)} placeholder="搜索书名、作者" />
        </form>
        {history.length > 0 && (
          <>
            <div className="list-section-title">搜索历史</div>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              {history.map(h => <button key={h} className="chip" onClick={() => doSearch(h)}>{h}</button>)}
            </div>
          </>
        )}
      </main>
    </>
  )
}
