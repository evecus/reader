import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { IconSearch, IconClose } from '../icons'
import { PageHeader } from '../App'

const HISTORY_KEY = 'reader_search_history'

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

  const clearHistory = () => {
    setHistory([])
    localStorage.setItem(HISTORY_KEY, '[]')
  }

  return (
    <>
      <PageHeader title="搜索书籍" subtitle="跨所有已启用书源搜索书名或作者" backTo={-1} />
      <form className="search-bar" onSubmit={e => { e.preventDefault(); doSearch(key) }}>
        <IconSearch />
        <input autoFocus value={key} onChange={e => setKey(e.target.value)} placeholder="输入书名、作者，回车搜索…" />
        <button className="btn btn-primary" type="submit">搜索</button>
      </form>
      {history.length > 0 && (
        <div style={{ marginTop: 26, maxWidth: 760 }}>
          <div className="section-title" style={{ justifyContent: 'space-between' }}>
            <span>搜索历史</span>
            <button className="icon-btn-sm" onClick={clearHistory} title="清空历史"><IconClose /></button>
          </div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
            {history.map(h => <button key={h} className="chip" onClick={() => doSearch(h)}>{h}</button>)}
          </div>
        </div>
      )}
    </>
  )
}
