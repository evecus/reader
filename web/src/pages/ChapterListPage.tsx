import { useEffect, useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconDownload, IconList } from '../icons'
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
    // 目录改为阅读页内通过 API 获取，避免长书章节 JSON 撑爆 URL
    const q = new URLSearchParams({
      origin, book_url: info.bookUrl,
      toc_url: tocUrl || info.bookUrl,
      title: c.title, name, author, index: String(index),
    })
    navigate(`/reader?${q}`)
  }

  const cacheAll = async () => {
    toast(`准备缓存 ${chapters.length} 章（逐章下载中）`)
    let ok = 0
    for (const [i, c] of chapters.entries()) {
      try {
        const r = await api.content(origin, c.url, info.bookUrl, c.title)
        await api.cacheChapter({
          book_url: info.bookUrl, chapter_url: c.url, title: c.title, content: r.content,
          origin, book_name: name, book_author: author, chapter_index: c.index ?? i,
        })
        ok++
      } catch { /* 跳过失败章节 */ }
    }
    toast(`缓存完成 ${ok}/${chapters.length} 章`)
  }

  const displayChapters = reverse ? [...chapters].reverse() : chapters

  return (
    <>
      <PageHeader
        title={name || '目录'}
        subtitle={loading ? '正在加载目录…' : `共 ${chapters.length} 章`}
        backTo={-1}
        actions={<>
          <button className={`btn ${reverse ? 'btn-soft' : 'btn-ghost'}`} onClick={() => setReverse(r => !r)}>
            <IconList /> {reverse ? '正序' : '倒序'}
          </button>
          <button className="btn btn-outline" onClick={cacheAll} disabled={loading || chapters.length === 0}>
            <IconDownload /> 缓存全部
          </button>
        </>}
      />
      {loading ? <div className="loading"><div className="spinner" />加载目录…</div>
        : chapters.length === 0 ? <div className="empty"><div className="empty-title">目录为空</div></div>
          : <div className="chapter-grid">
              {displayChapters.map((c, i) => (
                <div key={c.url + i} className={`chapter-item ${fromShelf && i === startChapter ? 'current' : ''}`}
                  onClick={() => openReader(reverse ? chapters.length - 1 - i : i)} title={c.title}>
                  <span className="ct-title">{c.title}</span>
                  {c.isVip && <span className="vip">VIP</span>}
                </div>
              ))}
            </div>}
    </>
  )
}
