import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { IconSearch, IconSettings } from '../icons'
import { PageHeader, useToast } from '../App'
import type { ShelfItem } from '../types'
import BookRow from './BookRow'

export default function ShelfPage() {
  const [items, setItems] = useState<ShelfItem[]>([])
  const [loading, setLoading] = useState(true)
  const navigate = useNavigate()
  const toast = useToast()

  const load = () => {
    api.listShelf().then(setItems).catch(e => toast(e.message)).finally(() => setLoading(false))
  }
  useEffect(load, [])

  return (
    <>
      <PageHeader
        title="我的书架"
        subtitle={loading ? '正在加载藏书…' : `共 ${items.length} 本藏书，点击书籍继续阅读`}
        actions={<>
          <button className="btn btn-ghost" onClick={() => navigate('/sources')}><IconSettings /> 书源管理</button>
          <button className="btn btn-primary" onClick={() => navigate('/search')}><IconSearch /> 搜索添加</button>
        </>}
      />
      {loading ? <div className="loading"><div className="spinner" />加载中…</div>
        : items.length === 0 ? (
          <div className="empty">
            <IconShelfBig />
            <div className="empty-title">书架还是空的</div>
            <p>去搜索添加一本书吧</p>
            <button className="btn btn-primary" onClick={() => navigate('/search')}>去搜书</button>
          </div>
        ) : (
          <>
            <div className="section-title">全部书籍 <span className="count">({items.length})</span></div>
            <div className="book-grid">
              {items.map(item => (
                <BookRow
                  key={item.id}
                  book={item.book}
                  subtitle={item.durChapterTitle ? `读到：${item.durChapterTitle}` : `读到第 ${item.durChapterIndex + 1} 章`}
                  onClick={() => navigate(`/chapters?origin=${encodeURIComponent(item.book.origin)}&url=${encodeURIComponent(item.book.bookUrl)}&toc_url=${encodeURIComponent(item.book.tocUrl)}&name=${encodeURIComponent(item.book.name)}&author=${encodeURIComponent(item.book.author)}&from=shelf&chapter=${item.durChapterIndex}`)}
                />
              ))}
            </div>
          </>
        )}
    </>
  )
}

function IconShelfBig() {
  return <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" width="54" height="54">
    <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/>
  </svg>
}
