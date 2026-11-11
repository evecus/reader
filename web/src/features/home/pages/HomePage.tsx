import { Link } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { ChevronRight, Play } from 'lucide-react';
import { useRuntime } from '../../../app/providers/runtime-context.tsx';
import { useAuthStore } from '../../../shared/stores/auth.store.ts';
import { BookCard, useCover } from '../../shelf/pages/ShelfPage.tsx';
import { useShelf } from '../../shelf/queries/shelf.queries.ts';
import type { ContinueReadingItem } from '../../../api/types.ts';
import '../styles/home.css';

export function greeting(hour: number): string {
  return hour < 6 ? '夜深了' : hour < 12 ? '早上好' : hour < 18 ? '下午好' : '晚上好';
}

function Resume({ book }: { book: ContinueReadingItem }) {
  const runtime = useRuntime();
  const src = useCover(runtime.api, book.coverUrl);
  const percent = Math.round(Math.max(0, Math.min(1, book.percentage)) * 100);
  return <Link className="home-hero" to={`/book/${encodeURIComponent(book.id)}`} aria-label={`继续阅读 ${book.title}`}>
    <span className="home-hero-cover" aria-hidden="true">{src ? <img src={src} alt="" /> : <b>{book.title}</b>}</span>
    <span className="home-hero-text">
      <strong>{book.title || '未命名'}</strong>
      {book.chapterTitle && <small>{book.chapterTitle}</small>}
      <span className="home-hero-foot">
        <span className="home-hero-action"><Play size={14} fill="currentColor" aria-hidden="true" />继续阅读</span>
        <span className="home-hero-meter">
          <span className="home-progress" aria-hidden="true"><i style={{ width: `${percent}%` }} /></span>
          <span className="home-hero-percent">已读 {percent}%</span>
        </span>
      </span>
    </span>
  </Link>;
}

/** 首页: where you left off in the book you are reading, then the shelf. */
export function HomePage() {
  const runtime = useRuntime();
  const user = useAuthStore(state => state.verifiedUser ?? state.session?.user ?? null);
  const reading = useQuery({
    queryKey: ['home', 'reading', runtime.api.baseUrl, user?.id],
    queryFn: ({ signal }) => runtime.api.continueReading(1, { signal }),
  });
  const shelf = useShelf(1, 'recent');
  const book = reading.data?.[0];
  const books = (shelf.data?.items ?? []).slice(0, 8);
  const name = user?.displayName || user?.username || '';
  return <div className="media-screen media-page home-page">
    <header className="home-header"><h1>{greeting(new Date().getHours())}{name ? `，${name}` : ''}</h1><p>从上次停下的地方继续</p></header>
    <div className="home-continue">
      {book
        ? <Resume book={book} />
        : <section className="home-empty" aria-label="继续阅读"><strong>还没有在读的书</strong><Link to="/shelf">去书架挑一本<ChevronRight size={16} aria-hidden="true" /></Link></section>}
    </div>
    {books.length > 0 && <section className="home-shelf" aria-label="我的书架">
      <div className="home-section"><h2>我的书架</h2><Link to="/shelf">全部<ChevronRight size={16} aria-hidden="true" /></Link></div>
      <div className="home-shelf-row">{books.map(item => <BookCard key={item.id} book={item} showAuthor={false} showProgress />)}</div>
    </section>}
  </div>;
}
