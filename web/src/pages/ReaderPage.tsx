import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconList, IconSettings } from '../icons'
import type { BookChapter } from '../types'

// 主题：淡黄为默认（参考 legado 阅读背景色 #F5ECD7）
const READER_THEMES = [
  { bg: '#f5ecd7', color: '#4a3c28', name: '淡黄' },
  { bg: '#c8e6a0', color: '#2e3b1f', name: '护眼绿' },
  { bg: '#ffffff', color: '#212121', name: '白' },
  { bg: '#2b2b2b', color: '#b0b0b0', name: '夜间' },
  { bg: '#d6e4f0', color: '#263845', name: '淡蓝' },
]

type ReadMode = 'page' | 'scroll'

export default function ReaderPage() {
  const [params] = useSearchParams()
  const origin = params.get('origin') || ''
  const bookUrl = params.get('book_url') || ''
  const tocUrl = params.get('toc_url') || ''
  const title0 = params.get('title') || ''
  const name = params.get('name') || ''
  const author = params.get('author') || ''
  const fromCache = params.get('from') === 'cache'
  const initialIndex = Number(params.get('index') || 0)

  const navigate = useNavigate()
  const toast = useToast()
  const [chapters, setChapters] = useState<BookChapter[]>([])
  const [chaptersLoading, setChaptersLoading] = useState(true)
  const [index, setIndex] = useState(initialIndex)
  const [content, setContent] = useState('')
  const [fromCacheFlag, setFromCacheFlag] = useState(false)
  const [loading, setLoading] = useState(true)
  const [showToolbar, setShowToolbar] = useState(false)
  const [showSettings, setShowSettings] = useState(false)
  const [fontSize, setFontSize] = useState(() => Number(localStorage.getItem('reader_font') || 19))
  const [themeIdx, setThemeIdx] = useState(() => Number(localStorage.getItem('reader_theme') || 0))
  const [mode, setMode] = useState<ReadMode>(() => (localStorage.getItem('reader_mode') as ReadMode) || 'page')
  // 翻页模式状态
  const [page, setPage] = useState(0)
  const [pageCount, setPageCount] = useState(1)
  // 章节切换后需要跳到的位置：'start' 首页 / 'end' 末页
  const pendingJump = useRef<'start' | 'end'>('start')
  const scrollRef = useRef<HTMLDivElement>(null)
  const colsWrapRef = useRef<HTMLDivElement>(null)
  const colsRef = useRef<HTMLDivElement>(null)

  const theme = READER_THEMES[themeIdx] || READER_THEMES[0]

  // 目录：优先在线获取；失败（如离线）回退到本地缓存的章节列表
  useEffect(() => {
    let cancelled = false
    setChaptersLoading(true)
    // 兼容旧链接：URL 里直接带 chapters JSON
    const legacy = params.get('chapters')
    let legacyChapters: BookChapter[] = []
    if (legacy) {
      try { legacyChapters = JSON.parse(legacy) } catch { /* ignore */ }
    }
    const fetchToc = async () => {
      if (legacyChapters.length > 0) return legacyChapters
      if (!bookUrl) return []
      try {
        const r = await api.chapters(origin, bookUrl, tocUrl || undefined, name, author)
        return r.chapters
      } catch {
        // 离线回退：按缓存的章节顺序打开
        try {
          const r = await api.listCacheChapters(bookUrl)
          return r.chapters.map(c => ({
            index: c.chapterIndex ?? 0, title: c.title, url: c.chapterUrl,
            isVolume: false, isVip: false,
          }))
        } catch { return [] }
      }
    }
    fetchToc().then(cs => {
      if (cancelled) return
      setChapters(cs)
      setChaptersLoading(false)
      if (legacyChapters.length === 0 && cs.length > 0 && initialIndex >= cs.length) {
        setIndex(0)
      }
    })
    return () => { cancelled = true }
  }, [origin, bookUrl, tocUrl, name, author])

  const chapter = chapters[index]
  const chapterTitle = chapter?.title || title0

  // 加载正文
  const load = useCallback(async () => {
    if (!chapter || !bookUrl) { setLoading(false); return }
    setLoading(true)
    setPage(0)
    try {
      const r = await api.content(origin, chapter.url, bookUrl, chapter.title)
      setContent(r.content)
      setFromCacheFlag(Boolean((r as { fromCache?: boolean }).fromCache))
      scrollRef.current?.scrollTo(0, 0)
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setLoading(false)
    }
  }, [origin, bookUrl, chapter, toast])

  useEffect(() => { load() }, [load])

  // 保存进度
  useEffect(() => {
    if (chapter && bookUrl) {
      api.updateProgress(bookUrl, origin, index, chapter.title).catch(() => {})
    }
  }, [index, chapter, bookUrl, origin])

  // ---- 翻页模式：CSS 多列分页 ----
  const paragraphs = content.includes('<p')
    ? Array.from(content.matchAll(/<p[^>]*>([\s\S]*?)<\/p>/gi)).map(m => m[1].trim()).filter(Boolean)
    : content.split(/\n+/).map(p => p.trim()).filter(Boolean)

  const measurePages = useCallback(() => {
    if (mode !== 'page') return
    const wrap = colsWrapRef.current, cols = colsRef.current
    if (!wrap || !cols || !content) return
    const w = wrap.clientWidth
    const gap = 48
    cols.style.columnWidth = `${w}px`
    cols.style.columnGap = `${gap}px`
    const n = Math.max(1, Math.round((cols.scrollWidth + gap) / (w + gap)))
    setPageCount(n)
    if (pendingJump.current === 'end') {
      setPage(n - 1)
      pendingJump.current = 'start'
    } else if (page >= n) {
      setPage(n - 1)
    }
  }, [mode, content, page])

  // 内容/字号/模式/章节变化后重新测量分页
  useEffect(() => {
    measurePages()
    // 等字体与布局稳定后再测一次
    const t = setTimeout(measurePages, 120)
    return () => clearTimeout(t)
  }, [measurePages, fontSize, loading, chaptersLoading])
  useEffect(() => {
    const onResize = () => measurePages()
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [measurePages])

  const goPage = useCallback((dir: 1 | -1) => {
    if (mode !== 'page') return
    if (dir === 1) {
      if (page < pageCount - 1) { setPage(p => p + 1); return }
      // 末页 → 下一章
      if (index < chapters.length - 1) { pendingJump.current = 'start'; setIndex(i => i + 1) }
    } else {
      if (page > 0) { setPage(p => p - 1); return }
      // 首页 → 上一章（进末页）
      if (index > 0) { pendingJump.current = 'end'; setIndex(i => i - 1) }
    }
  }, [mode, page, pageCount, index, chapters.length])

  // 点击分区：左 1/3 上一页/上一章，右 1/3 下一页/下一章，中间切换工具栏（对齐 legado）
  const onTapZone = (e: React.MouseEvent | React.TouchEvent) => {
    const el = e.currentTarget as HTMLElement
    const rect = el.getBoundingClientRect()
    const clientX = 'touches' in e ? (e as React.TouchEvent).touches[0]?.clientX ?? 0 : (e as React.MouseEvent).clientX
    const x = (clientX - rect.left) / rect.width
    if (x < 0.33) {
      if (mode === 'page') goPage(-1)
      else if (index > 0) { pendingJump.current = 'end'; setIndex(i => i - 1) }
    } else if (x > 0.67) {
      if (mode === 'page') goPage(1)
      else if (index < chapters.length - 1) { pendingJump.current = 'start'; setIndex(i => i + 1) }
    } else {
      setShowToolbar(s => !s)
    }
  }

  // 翻页模式：滚动容器横向平移
  useEffect(() => {
    if (mode !== 'page') return
    const wrap = colsWrapRef.current
    if (!wrap) return
    const w = wrap.clientWidth
    wrap.scrollTo({ left: page * (w + 48), behavior: 'auto' })
  }, [page, mode, pageCount, loading])

  const changeFontSize = (delta: number) => {
    const next = Math.max(14, Math.min(32, fontSize + delta))
    setFontSize(next); localStorage.setItem('reader_font', String(next))
  }
  const changeTheme = (i: number) => { setThemeIdx(i); localStorage.setItem('reader_theme', String(i)) }
  const changeMode = (m: ReadMode) => {
    setMode(m); localStorage.setItem('reader_mode', m)
    setPage(0); pendingJump.current = 'start'
  }

  // 缓存当前章（带书籍元数据，供缓存页按书展示）
  const cacheCurrent = async () => {
    if (!chapter) return
    try {
      await api.cacheChapter({
        book_url: bookUrl, chapter_url: chapter.url, title: chapter.title, content,
        origin, book_name: name, book_author: author, chapter_index: index,
      })
      toast('已缓存本章')
    } catch (e) { toast((e as Error).message) }
  }

  return (
    <div className="reader" style={{ background: theme.bg, color: theme.color }}>
      <div className="reader-topbar" style={{ display: showToolbar ? 'flex' : 'none' }}>
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title">{chapterTitle}</div>
        <button className="icon-btn" onClick={() => navigate(`/chapters?origin=${encodeURIComponent(origin)}&url=${encodeURIComponent(bookUrl)}&toc_url=${encodeURIComponent(tocUrl)}&name=${encodeURIComponent(name)}&author=${encodeURIComponent(author)}`)}><IconList /></button>
        <button className="icon-btn" onClick={() => setShowSettings(true)}><IconSettings /></button>
      </div>

      {mode === 'page' ? (
        <div className="reader-paged" style={{ fontSize: `${fontSize}px`, color: theme.color }}
          onClick={onTapZone}>
          <div className="reader-cols-scroll" ref={colsWrapRef}>
            {loading || chaptersLoading
              ? <div className="loading" style={{ color: theme.color }}><div className="spinner" />加载正文…</div>
              : <div className="reader-cols" ref={colsRef}>
                  {paragraphs.length === 0
                    ? <p style={{ textIndent: 0 }}>暂无正文内容</p>
                    : paragraphs.map((p, i) => <p key={i}>{p}</p>)}
                </div>}
          </div>
          {!loading && !chaptersLoading && paragraphs.length > 0 && (
            <div className="reader-page-indicator" style={{ color: theme.color }}>
              {page + 1} / {pageCount}
              {fromCacheFlag && <span className="reader-cache-tag">缓存</span>}
            </div>
          )}
        </div>
      ) : (
        <div className="reader-content" ref={scrollRef}
          style={{ fontSize: `${fontSize}px`, paddingTop: showToolbar ? 16 : 48, color: theme.color }}
          onClick={onTapZone}>
          <div className="reader-inner">
            {loading || chaptersLoading ? <div className="loading" style={{ color: theme.color }}><div className="spinner" />加载正文…</div>
              : paragraphs.length === 0 ? <p>暂无正文内容</p>
              : paragraphs.map((p, i) => <p key={i}>{p}</p>)}
          </div>
        </div>
      )}

      <div className="reader-bottombar" style={{ display: showToolbar ? 'flex' : 'none' }}>
        <button className="btn btn-ghost" disabled={index <= 0} onClick={() => { pendingJump.current = 'end'; setIndex(i => i - 1) }}>上一章</button>
        <button className="btn btn-ghost" onClick={cacheCurrent}>缓存本章</button>
        <button className="btn btn-ghost" disabled={index >= chapters.length - 1} onClick={() => { pendingJump.current = 'start'; setIndex(i => i + 1) }}>下一章</button>
      </div>

      {showSettings && (
        <div className="dialog-mask" onClick={() => setShowSettings(false)}>
          <div className="reader-setting" onClick={e => e.stopPropagation()}>
            <h2 style={{ fontSize: 18, marginBottom: 14 }}>阅读设置</h2>
            <div style={{ fontSize: 13, color: '#757575' }}>字号</div>
            <div className="font-controls">
              <button onClick={() => changeFontSize(-1)}>A-</button>
              <span style={{ flex: 1, textAlign: 'center', fontWeight: 600 }}>{fontSize}px</span>
              <button onClick={() => changeFontSize(1)}>A+</button>
            </div>
            <div style={{ fontSize: 13, color: '#757575', marginTop: 12 }}>翻页模式</div>
            <div className="mode-controls" style={{ marginTop: 10 }}>
              <button className={`mode-btn ${mode === 'page' ? 'active' : ''}`} onClick={() => changeMode('page')}>翻页</button>
              <button className={`mode-btn ${mode === 'scroll' ? 'active' : ''}`} onClick={() => changeMode('scroll')}>滚动</button>
            </div>
            <div style={{ fontSize: 13, color: '#757575', marginTop: 12 }}>背景主题</div>
            <div className="theme-dots" style={{ marginTop: 10 }}>
              {READER_THEMES.map((t, i) => (
                <button key={i} className={`theme-dot ${themeIdx === i ? 'active' : ''}`}
                  style={{ background: t.bg }} title={t.name} onClick={() => changeTheme(i)} />
              ))}
            </div>
            <button className="btn btn-primary btn-block" style={{ marginTop: 22 }} onClick={() => setShowSettings(false)}>完成</button>
          </div>
        </div>
      )}
    </div>
  )
}
