import type { Book } from '../types'

interface Props {
  book: Book
  onClick?: () => void
  subtitle?: string
  extra?: React.ReactNode
}

export default function BookRow({ book, onClick, subtitle, extra }: Props) {
  const cover = book.coverUrl ? (
    <img src={book.coverUrl} alt="" onError={e => {
      (e.currentTarget.parentElement as HTMLElement).textContent = book.name.slice(0, 4)
    }} />
  ) : book.name.slice(0, 4)

  return (
    <div className="book-card" onClick={onClick} title={book.name}>
      <div className="book-card-cover">
        {cover}
        {book.origins?.length > 1 && <span className="book-card-badge">{book.origins.length} 源</span>}
      </div>
      <div className="book-card-body">
        <div className="book-card-title">{book.name}</div>
        <div className="book-card-author">{book.author || '未知作者'}</div>
        <div className="book-card-meta">{subtitle || [book.kind, book.wordCount, book.lastChapter].filter(Boolean).join(' · ')}</div>
        {extra}
      </div>
    </div>
  )
}
