import { useState, type FormEvent } from 'react';
import { Link, useLocation, useNavigate } from 'react-router-dom';
import { BookOpen, House, Search, UserRound, type LucideIcon } from 'lucide-react';
import { useAuthStore } from '../shared/stores/auth.store.ts';
import { useSyncStore } from '../shared/stores/sync.store.ts';
import type { SyncStatus } from '../core/sync.ts';
import { useRuntime } from './providers/runtime-context.tsx';

/**
 * hidden: the book owns the whole screen.
 * top:    a top-level destination; the bar shows everywhere.
 * push:   a page reached from a top-level one (detail, settings, library files):
 *         on a phone it replaces the bar with its own back arrow, on desktop the
 *         sidebar stays.
 */
export type NavMode = 'hidden' | 'top' | 'push';

export function navMode(pathname: string): NavMode {
  if (pathname.startsWith('/book/')) return 'hidden';
  if (pathname === '/' || pathname === '/home' || pathname === '/shelf' || pathname === '/me') return 'top';
  return 'push';
}

interface Destination { to: string; label: string; icon: LucideIcon; current(path: string): boolean }
const destinations: Destination[] = [
  { to: '/home', label: '首页', icon: House, current: path => path === '/home' },
  { to: '/shelf', label: '书架', icon: BookOpen, current: path => /^\/(shelf|library|sources)(\/|$)/.test(path) },
  { to: '/me', label: '我的', icon: UserRound, current: path => path === '/me' || path.startsWith('/me/') },
];

/** The one navigation: a tab bar on a phone, a sidebar from 701px up. */
export function AppNav({ mode }: { mode: Exclude<NavMode, 'hidden'> }) {
  const { pathname } = useLocation();
  const runtime = useRuntime();
  const user = useAuthStore(state => state.verifiedUser ?? state.session?.user ?? null);
  const name = user?.displayName || user?.username || '';
  const sync = useSyncStore(state => state.status.state);
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const search = (event: FormEvent) => { event.preventDefault(); const q = query.trim(); if (q) navigate(`/search?q=${encodeURIComponent(q)}`); };
  const connection = connectionLabel(sync);
  return <nav className="app-nav" aria-label="主导航" data-mode={mode}>
    <Link className="app-nav-brand" to="/" aria-label="reader. 首页">reader.</Link>
    {/* Desktop only: one search across the book library (the page is /search). */}
    <form className="app-nav-search" role="search" onSubmit={search}>
      <Search size={16} aria-hidden="true" />
      <input type="search" aria-label="搜索书籍" placeholder="搜索书籍" value={query} maxLength={200} onInput={event => setQuery(event.currentTarget.value)} />
    </form>
    <ul className="app-nav-list">
      {destinations.map(({ to, label, icon: Icon, current }) => {
        const active = current(pathname);
        return <li key={to}>
          <Link className="app-nav-item" to={to} aria-current={active ? 'page' : undefined}>
            <span className="app-nav-icon" aria-hidden="true"><Icon size={22} strokeWidth={active ? 2.1 : 1.8} /></span>
            <span className="app-nav-label">{label}</span>
          </Link>
        </li>;
      })}
    </ul>
    {user && <Link className="app-nav-user" to="/me" aria-label="我的账号">
      <span className="app-nav-avatar" aria-hidden="true">{Array.from(name)[0]}</span>
      <span className="app-nav-user-text"><strong>{name}</strong><small title={host(runtime.api.baseUrl)}><i className="app-nav-status" data-state={connection.state} aria-hidden="true" />{connection.label}</small></span>
    </Link>}
  </nav>;
}

function host(baseUrl: string): string { try { return new URL(baseUrl).host; } catch { return baseUrl; } }

/** The sync engine's view of the server, in the sidebar's words. */
export function connectionLabel(state: SyncStatus['state']): { state: 'online' | 'offline' | 'error'; label: string } {
  if (state === 'offline') return { state: 'offline', label: '离线' };
  if (state === 'error') return { state: 'error', label: '同步出错' };
  if (state === 'syncing') return { state: 'online', label: '正在同步' };
  return { state: 'online', label: '已连接' };
}
