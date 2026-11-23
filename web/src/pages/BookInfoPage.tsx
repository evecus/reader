import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconBook, IconDownload } from '../icons'
import type { Book } from '../types'

export default function BookInfoPage() {
  const [params] = useSearchParams()
  const origin = params.get('origin') || ''
  const url = params.get('url') || ''
  const navigate = useNavigate()
  const toast = useToast()
  const [book, setBook] = useState<Book>(() => ({
    bookUrl: url, origin, originName: '',
    name: params.get('name') || '', author: params.get('author') || '',
    kind: params.get('kind') || '', wordCount: params.get('wordcount') || '',
    lastChapter: params.get('lastchapter') || '', intro: '',
    coverUrl: params.get('cover') || '', tocUrl: '', origins: [origin],
  }))
  const [loading, setLoading] = useState(true)
  const [inShelf, setInShelf] = useState(false)

  useEffect(() => {
    Promise.all([api.bookInfo(origin, url), api.listShelf()])
      .then(([b, shelf]) => {
        setBook(b)
        setInShelf(shelf.some(s => s.book.bookUrl === b.bookUrl && s.book.origin === b.origin))
      })
      .catch(e => toast(e.message))
      .finally(() => setLoading(false))
  }, [origin, url])

  const goChapters = () => navigate(`/chapters?origin=${encodeURIComponent(origin)}&url=${encodeURIComponent(book.bookUrl || url)}&toc_url=${encodeURIComponent(book.tocUrl)}&name=${encodeURIComponent(book.name)}&author=${encodeURIComponent(book.author)}`)

  const [downloading, setDownloading] = useState(false)
  const downloadAll = async () => {
    if (downloading) return
    setDownloading(true)
    try {
      await api.startDownload({
        origin: book.origin || origin,
        book_url: book.bookUrl || url,
        toc_url: book.tocUrl,
        name: book.name,
        author: book.author,
      })
      toast('已开始后台下载，可在「缓存」页查看进度')
      navigate('/cache')
    } catch (e) { toast((e as Error).message); setDownloading(false) }
  }

  const toggleShelf = async () => {
    try {
      if (inShelf) {
        await api.removeShelf(book.bookUrl, book.origin)
        setInShelf(false); toast('已从书架移除')
      } else {
        await api.addShelf({
          id: `${book.origin}:${book.bookUrl}`, book,
          durChapterIndex: 0, durChapterTitle: '',
          addedAt: Date.now(), updatedAt: Date.now(),
        })
        setInShelf(true); toast('已加入书架')
      }
    } catch (e) { toast((e as Error).message) }
  }

  const cover = book.coverUrl
    ? <img src={book.coverUrl} alt="" onError={e => { (e.currentTarget as HTMLImageElement).style.display = 'none' }} />
    : book.name.slice(0, 4)

  return (
    <>
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title">书籍信息</div>
      </header>
      <main className="page">
        {loading && <div className="loading"><div className="spinner" />加载详情…</div>}
        <div className="detail-header">
          <div style={{ display: 'flex', gap: 16 }}>
            <div className="book-cover" style={{ width: 96, height: 132, fontSize: 16 }}>{cover}</div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div className="detail-title">{book.name}</div>
              <div className="detail-meta"><span>{book.author || '未知作者'}</span><span>{book.kind}</span></div>
              <div className="detail-meta"><span>{book.wordCount}</span></div>
              <div style={{ fontSize: 12, color: 'var(--text-secondary)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                最新：{book.lastChapter || '暂无'}
              </div>
              <div style={{ fontSize: 12, color: 'var(--primary)', marginTop: 4 }}>来源：{book.originName}</div>
            </div>
          </div>
          <div className="detail-actions">
            <button className={`btn ${inShelf ? 'btn-ghost' : 'btn-outline'}`} onClick={toggleShelf}>
              {inShelf ? '已在书架' : '加入书架'}
            </button>
            <button className="btn btn-outline" onClick={downloadAll} disabled={downloading}>
              <IconDownload style={{ width: 16, height: 16 }} /> {downloading ? '下载中' : '下载全书'}
            </button>
            <button className="btn btn-primary" onClick={goChapters}><IconBook /> 开始阅读</button>
          </div>
        </div>
        <div className="detail-intro">
          <h3>简介</h3>
          <p>{book.intro || '暂无简介'}</p>
        </div>
      </main>
    </>
  )
}
