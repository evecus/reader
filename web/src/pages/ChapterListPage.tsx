import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconDownload, IconList } from '../icons'
import type { BookChapter } from '../types'

export default function ChapterListPage() {
  const [params] = useSearchParams()
  const origin = params.get('origin') || ''
  const url = params.get('url') || ''
  const tocUrl = params.get('toc_url') || ''
  const name = params.get('name') || ''
  const author = params.get('author') || ''
  const fromShelf = params.get('from') === 'shelf'
  const startChapter = Number(params.get('chapter') || 0)
  const navigate = useNavigate()
  const toast = useToast()
  const [chapters, setChapters] = useState<BookChapter[]>([])
  const [loading, setLoading] = useState(true)
  const [reverse, setReverse] = useState(false)

  const info = useMemo(() => ({
    origin, url, tocUrl, name, author,
    bookUrl: url,
  }), [origin, url, tocUrl, name, author])

  useEffect(() => {
    api.chapters(origin, url, tocUrl || undefined, name, author)
      .then(r => setChapters(r.chapters))
      .catch(e => toast(e.message))
      .finally(() => setLoading(false))
  }, [origin, url, tocUrl])

  const openReader = (index: number) => {
    const c = chapters[index]
    const q = new URLSearchParams({
      origin, url: c.url, book_url: info.bookUrl,
      title: c.title, name, author,
      chapters: JSON.stringify(chapters), index: String(index),
    })
    navigate(`/reader?${q}`)
  }

  const cacheAll = async () => {
    toast(`准备缓存 ${chapters.length} 章（逐章下载中）`)
    let ok = 0
    for (const c of chapters) {
      try {
        const r = await api.content(origin, c.url, info.bookUrl, c.title)
        await api.cacheChapter({ book_url: info.bookUrl, chapter_url: c.url, title: c.title, content: r.content })
        ok++
      } catch { /* 跳过失败章节 */ }
    }
    toast(`缓存完成 ${ok}/${chapters.length} 章`)
  }

  const displayChapters = reverse ? [...chapters].reverse() : chapters

  return (
    <>
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title">{name || '目录'} <span style={{ fontSize: 13, opacity: .8 }}>({chapters.length})</span></div>
        <button className="icon-btn" onClick={() => setReverse(r => !r)} title="倒序"><IconList /></button>
        <button className="icon-btn" onClick={cacheAll} title="缓存全部"><IconDownload /></button>
      </header>
      <main className="page no-pad" style={{ padding: '0 12px 12px' }}>
        {loading ? <div className="loading"><div className="spinner" />加载目录…</div>
          : chapters.length === 0 ? <div className="empty"><div>目录为空</div></div>
          : <div className="chapter-list" style={{ marginTop: 12 }}>
              {displayChapters.map((c, i) => (
                <div key={c.url + i} className={`chapter-item ${fromShelf && i === startChapter ? 'current' : ''}`}
                  onClick={() => openReader(reverse ? chapters.length - 1 - i : i)}>
                  <span>{c.title}</span>
                  {c.isVip && <span className="vip">VIP</span>}
                </div>
              ))}
            </div>}
      </main>
    </>
  )
}
