import { createContext, useCallback, useContext, useState } from 'react'
import { Route, Routes, useLocation, useNavigate } from 'react-router-dom'
import { IconBookmark, IconExplore, IconShelf, IconUser } from './icons'
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

function Shell() {
  const [toastMsg, setToastMsg] = useState('')
  const navigate = useNavigate()
  const location = useLocation()
  const toast = useCallback((msg: string) => {
    setToastMsg(msg)
    setTimeout(() => setToastMsg(''), 2500)
  }, [])

  const navItems = [
    { path: '/shelf', label: '书架', icon: IconShelf },
    { path: '/explore', label: '发现', icon: IconExplore },
    { path: '/cache', label: '缓存', icon: IconBookmark },
    { path: '/sources', label: '书源', icon: IconUser },
  ]
  const showNav = !['/reader', '/book', '/chapters', '/search'].some(p => location.pathname.startsWith(p))

  return (
    <ToastContext.Provider value={{ toast }}>
      <div className="app-shell">
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
        {showNav && (
          <nav className="bottom-nav">
            {navItems.map(item => {
              const active = location.pathname === item.path
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
        )}
      </div>
      {toastMsg && <div className="toast">{toastMsg}</div>}
    </ToastContext.Provider>
  )
}

export default function App() {
  return <Shell />
}
