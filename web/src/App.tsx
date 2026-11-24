import { createContext, useCallback, useContext, useEffect, useState } from 'react'
import type { ReactNode } from 'react'
import { Route, Routes, useLocation, useNavigate } from 'react-router-dom'
import { IconBook, IconExplore, IconMenu, IconShelf, IconBookmark, IconUser, IconSearch, IconBack } from './icons'
import ShelfPage from './pages/ShelfPage'
import SearchPage from './pages/SearchPage'
import ResultPage from './pages/ResultPage'
import BookInfoPage from './pages/BookInfoPage'
import ChapterListPage from './pages/ChapterListPage'
import ReaderPage from './pages/ReaderPage'
import SourcesPage from './pages/SourcesPage'
import CachePage from './pages/CachePage'

// 全局 toast
interface ToastCtx { toast: (msg: string) => void }
export const ToastContext = createContext<ToastCtx>({ toast: () => {} })
export const useToast = () => useContext(ToastContext).toast

// 通用页面头部：标题 + 副标题 + 右侧操作区
export function PageHeader({ title, subtitle, actions, backTo }: {
  title: string
  subtitle?: string
  actions?: ReactNode
  backTo?: string | number
}) {
  const navigate = useNavigate()
  return (
    <div className="page-header">
      <div className="ph-text">
        <h1>{title}</h1>
        {subtitle && <p>{subtitle}</p>}
      </div>
      {actions && <div className="ph-actions">
        {backTo !== undefined && (
          <button className="btn btn-ghost" onClick={() => typeof backTo === 'number' ? navigate(backTo) : navigate(backTo)} title="返回">
            <IconBack /> 返回
          </button>
        )}
        {actions}
      </div>}
    </div>
  )
}

const NAV_ITEMS = [
  { path: '/shelf', label: '书架', icon: IconShelf },
  { path: '/explore', label: '发现', icon: IconExplore },
  { path: '/cache', label: '离线缓存', icon: IconBookmark },
  { path: '/sources', label: '书源管理', icon: IconUser },
]

const PAGE_TITLES: Record<string, string> = {
  '/': '书架', '/shelf': '书架', '/search': '搜索', '/result': '搜索结果',
  '/book': '书籍详情', '/chapters': '目录', '/explore': '发现',
  '/sources': '书源管理', '/cache': '离线缓存',
}

function matchNav(path: string, current: string) {
  if (path === '/shelf') return ['/', '/shelf', '/search', '/result', '/book', '/chapters'].includes(current)
  return current === path || current.startsWith(path + '/')
}

function AppRoutes() {
  return (
    <Routes>
      <Route path="/" element={<ShelfPage />} />
      <Route path="/shelf" element={<ShelfPage />} />
      <Route path="/search" element={<SearchPage />} />
      <Route path="/result" element={<ResultPage />} />
      <Route path="/book" element={<BookInfoPage />} />
      <Route path="/chapters" element={<ChapterListPage />} />
      <Route path="/reader" element={<ReaderPage />} />
      <Route path="/explore" element={<ResultPage explore />} />
      <Route path="/sources" element={<SourcesPage />} />
      <Route path="/cache" element={<CachePage />} />
    </Routes>
  )
}

function Shell() {
  const [toastMsg, setToastMsg] = useState('')
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const navigate = useNavigate()
  const location = useLocation()
  const toast = useCallback((msg: string) => {
    setToastMsg(msg)
    setTimeout(() => setToastMsg(''), 2500)
  }, [])

  // 切换页面时关闭移动端抽屉
  useEffect(() => { setSidebarOpen(false) }, [location.pathname])

  // 阅读页全屏沉浸，不渲染侧边栏与顶栏
  const immersive = location.pathname === '/reader'
  const pageTitle = PAGE_TITLES[location.pathname] || '阅读管理'

  if (immersive) {
    return (
      <ToastContext.Provider value={{ toast }}>
        <AppRoutes />
        {toastMsg && <div className="toast">{toastMsg}</div>}
      </ToastContext.Provider>
    )
  }

  return (
    <ToastContext.Provider value={{ toast }}>
      <div className="layout">
        {sidebarOpen && <div className="sidebar-overlay" onClick={() => setSidebarOpen(false)} />}
        <aside className={`sidebar ${sidebarOpen ? 'open' : ''}`}>
          <div className="sidebar-brand" onClick={() => navigate('/shelf')}>
            <div className="brand-icon"><IconBook /></div>
            <div>
              <div className="brand-name">Reader</div>
              <div className="brand-sub">阅读管理面板</div>
            </div>
          </div>
          <nav className="sidebar-nav">
            <div className="nav-label">内容管理</div>
            {NAV_ITEMS.map(item => {
              const active = matchNav(item.path, location.pathname)
              const Icon = item.icon
              return (
                <button key={item.path} className={`nav-item ${active ? 'active' : ''}`}
                  onClick={() => navigate(item.path)}>
                  <Icon />
                  <span>{item.label}</span>
                </button>
              )
            })}
          </nav>
          <div className="sidebar-footer">
            <div className="status-line"><span className="status-dot" /> 服务运行中</div>
            <div style={{ marginTop: 6, opacity: .75 }}>本地单用户 · v0.1.0</div>
          </div>
        </aside>

        <div className="main-wrap">
          <header className="topbar">
            <button className="menu-toggle" onClick={() => setSidebarOpen(true)} title="菜单"><IconMenu /></button>
            <div className="breadcrumb">
              <span>管理面板</span><span className="sep">/</span><span className="current">{pageTitle}</span>
            </div>
            <div className="topbar-spacer" />
            <div className="topbar-online"><span className="status-dot" /><span>在线</span></div>
            <button className="topbar-action" onClick={() => navigate('/search')} title="搜索"><IconSearch /></button>
          </header>
          <main className="main-content">
            <AppRoutes />
          </main>
        </div>
      </div>
      {toastMsg && <div className="toast">{toastMsg}</div>}
    </ToastContext.Provider>
  )
}

export default function App() {
  return <Shell />
}
