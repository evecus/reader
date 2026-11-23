import type { Book } from '../types'

interface Props {
  book: Book
  onClick?: () => void
  subtitle?: string
  extra?: React.ReactNode
}

export default function BookRow({ book, onClick, subtitle, extra }: Props) {
  const cover = book.coverUrl ? (
    <div className="book-cover"><img src={book.coverUrl} alt="" onError={e => {
      (e.currentTarget.parentElement as HTMLElement).innerHTML = book.name.slice(0, 4)
    }} /></div>
  ) : <div className="book-cover">{book.name.slice(0, 4)}</div>

  return (
    <div className="book-row" onClick={onClick}>
      {cover}
      <div className="book-info">
        <div className="name">
          {book.name}
          {book.origins?.length > 1 && <span className="tag">{book.origins.length}源</span>}
        </div>
        <div className="author">{book.author || '未知作者'}</div>
        <div className="meta">{subtitle || [book.kind, book.wordCount, book.lastChapter].filter(Boolean).join(' · ')}</div>
        {book.intro && <div className="intro">{book.intro}</div>}
        {extra}
      </div>
    </div>
  )
}
