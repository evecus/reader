import { useEffect, useState, type FormEvent } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Link, useSearchParams } from 'react-router-dom';
import { ChevronRight, Search } from 'lucide-react';
import { useRuntime } from '../../../app/providers/runtime-context.tsx';
import { useSettingsStore } from '../../../shared/stores/settings.store.ts';
import { BookCard } from '../../shelf/pages/ShelfPage.tsx';
import '../../home/styles/home.css';
import '../styles/search.css';

const BOOK_LIMIT = 12;

/** 搜索: one query across the book library, with a link to the full library list. */
export function SearchPage() {
  const runtime = useRuntime();
  const [params, setParams] = useSearchParams();
  const query = (params.get('q') ?? '').trim();
  const [input, setInput] = useState(query);
  useEffect(() => setInput(query), [query]);
  const settings = useSettingsStore(state => state.settings);
  const user = runtime.api.currentSession()?.user.id;
  const books = useQuery({
    queryKey: ['search', 'books', runtime.api.baseUrl, user, query],
    queryFn: ({ signal }) => runtime.api.listBooks({ scope: 'library', search: query, page: 1, pageSize: BOOK_LIMIT }, { signal }),
    enabled: query.length > 0,
  });
  const submit = (event: FormEvent) => { event.preventDefault(); const q = input.trim(); setParams(q ? { q } : {}); };
  return <div className="media-screen media-page search-page">
    <header className="media-heading"><div className="media-page-heading"><h1>搜索</h1></div></header>
    <form className="search-page-form" role="search" onSubmit={submit}>
      <Search size={18} aria-hidden="true" />
      <input type="search" name="q" aria-label="搜索书籍" placeholder="搜索书名、作者" value={input} maxLength={200} onInput={event => setInput(event.currentTarget.value)} />
      <button type="submit" className="media-primary" disabled={!input.trim()}>搜索</button>
    </form>
    {!query ? <p className="search-page-hint">输入书名或作者，搜索书库中的书籍。</p> :
      <section className="search-page-section" aria-label="书籍搜索结果">
        <header><h2>书籍{books.data ? <small>{books.data.total}</small> : null}</h2>
          {books.data && books.data.total > BOOK_LIMIT && <Link to={`/library?q=${encodeURIComponent(query)}`}>在书库查看全部<ChevronRight size={16} aria-hidden="true" /></Link>}</header>
        {books.isPending ? <p role="status">正在搜索书籍…</p>
          : books.error ? <p className="media-error" role="alert">书籍搜索失败：{books.error.message} <button type="button" onClick={() => void books.refetch()}>重试</button></p>
          : books.data.items.length === 0 ? <p className="search-page-empty">没有找到书籍</p>
          : <div className="book-grid" data-density={settings.shelfDensity} data-show-author={String(settings.shelfShowAuthor)}>
            {books.data.items.map(book => <BookCard key={book.id} book={book} showAuthor={settings.shelfShowAuthor} showProgress={false} />)}
          </div>}
      </section>}
  </div>;
}
