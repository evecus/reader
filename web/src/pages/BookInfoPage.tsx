import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconBook, IconDownload } from '../icons'
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
      toast('已开始后台下载，可在「离线缓存」页查看进度')
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
      <PageHeader title="书籍详情" backTo={-1}
        subtitle={loading ? '正在加载详情…' : '书籍信息与阅读操作'} />
      {loading && <div className="loading"><div className="spinner" />加载详情…</div>}
      <div className="detail-layout">
        <div className="detail-cover">{cover}</div>
        <div className="detail-main">
          <h2>{book.name}</h2>
          <div className="detail-meta">
            <span>作者 <b>{book.author || '未知'}</b></span>
            <span>分类 <b>{book.kind || '—'}</b></span>
            <span>字数 <b>{book.wordCount || '—'}</b></span>
          </div>
          <div className="detail-meta">
            <span>最新章节 <b>{book.lastChapter || '暂无'}</b></span>
          </div>
          <div className="detail-origin">来源：{book.originName || origin}</div>
          <div className="detail-actions">
            <button className={`btn ${inShelf ? 'btn-soft' : 'btn-outline'}`} onClick={toggleShelf}>
              {inShelf ? '✓ 已在书架' : '+ 加入书架'}
            </button>
            <button className="btn btn-ghost" onClick={downloadAll} disabled={downloading}>
              <IconDownload /> {downloading ? '下载中' : '下载全书'}
            </button>
            <button className="btn btn-primary" onClick={goChapters}><IconBook /> 开始阅读</button>
          </div>
        </div>
      </div>
      <div className="detail-intro">
        <h3>内容简介</h3>
        <p>{book.intro || '暂无简介'}</p>
      </div>
    </>
  )
}
