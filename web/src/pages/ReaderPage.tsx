import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconList, IconSettings } from '../icons'
import type { BookChapter } from '../types'

const READER_THEMES = [
  { bg: '#c8e6a0', color: '#2e3b1f', name: '护眼绿' },
  { bg: '#ffffff', color: '#212121', name: '白' },
  { bg: '#f5ecd7', color: '#4a3c28', name: '米黄' },
  { bg: '#2b2b2b', color: '#b0b0b0', name: '夜间' },
  { bg: '#d6e4f0', color: '#263845', name: '淡蓝' },
]

export default function ReaderPage() {
  const [params] = useSearchParams()
  const origin = params.get('origin') || ''
  const url = params.get('url') || ''
  const bookUrl = params.get('book_url') || ''
  const title = params.get('title') || ''
  const name = params.get('name') || ''
  const author = params.get('author') || ''
  let chapters: BookChapter[] = []
  try { chapters = JSON.parse(params.get('chapters') || '[]') } catch { /* ignore */ }
  const initialIndex = Number(params.get('index') || 0)

  const navigate = useNavigate()
  const toast = useToast()
  const [index, setIndex] = useState(initialIndex)
  const [content, setContent] = useState('')
  const [loading, setLoading] = useState(true)
  const [showToolbar, setShowToolbar] = useState(false)
  const [showSettings, setShowSettings] = useState(false)
  const [fontSize, setFontSize] = useState(() => Number(localStorage.getItem('reader_font') || 19))
  const [themeIdx, setThemeIdx] = useState(() => Number(localStorage.getItem('reader_theme') || 0))
  const scrollRef = useRef<HTMLDivElement>(null)

  const chapter = chapters[index]

  const load = useCallback(async () => {
    if (!chapter) { setLoading(false); return }
    setLoading(true)
    api.content(origin, chapter.url, bookUrl, chapter.title)
      .then(r => { setContent(r.content); scrollRef.current?.scrollTo(0, 0) })
      .catch(e => toast(e.message))
      .finally(() => setLoading(false))
  }, [origin, bookUrl, chapter, toast])

  useEffect(() => { load() }, [load])

  // 保存进度
  useEffect(() => {
    if (chapter && bookUrl) {
      api.updateProgress(bookUrl, origin, index, chapter.title).catch(() => {})
    }
  }, [index, chapter, bookUrl, origin])

  // 缓存当前章
  const cacheCurrent = async () => {
    if (!chapter) return
    try {
      await api.cacheChapter({ book_url: bookUrl, chapter_url: chapter.url, title: chapter.title, content })
      toast('已缓存本章')
    } catch (e) { toast((e as Error).message) }
  }

  const theme = READER_THEMES[themeIdx] || READER_THEMES[0]
  const changeFontSize = (delta: number) => {
    const next = Math.max(14, Math.min(32, fontSize + delta))
    setFontSize(next); localStorage.setItem('reader_font', String(next))
  }
  const changeTheme = (i: number) => { setThemeIdx(i); localStorage.setItem('reader_theme', String(i)) }

  // 正文可能是 <p>段落</p> HTML，也可能是纯文本逐行
  const paragraphs = content.includes('<p')
    ? Array.from(content.matchAll(/<p[^>]*>([\s\S]*?)<\/p>/gi)).map(m => m[1].trim()).filter(Boolean)
    : content.split(/\n+/).map(p => p.trim()).filter(Boolean)

  return (
    <div className="reader" style={{ background: theme.bg, color: theme.color }}>
      <div className="reader-topbar" style={{ display: showToolbar ? 'flex' : 'none' }}>
        <button className="icon-btn" onClick={() => navigate(-1)}><IconBack /></button>
        <div className="title">{chapter?.title || title}</div>
        <button className="icon-btn" onClick={() => navigate(`/chapters?origin=${encodeURIComponent(origin)}&url=${encodeURIComponent(bookUrl)}&name=${encodeURIComponent(name)}&author=${encodeURIComponent(author)}`)}><IconList /></button>
        <button className="icon-btn" onClick={() => setShowSettings(true)}><IconSettings /></button>
      </div>

      <div className="reader-content" ref={scrollRef}
        style={{ fontSize: `${fontSize}px`, paddingTop: showToolbar ? 16 : 48 }}
        onClick={() => setShowToolbar(s => !s)}>
        <div className="reader-inner">
          {loading ? <div className="loading" style={{ color: theme.color }}><div className="spinner" />加载正文…</div>
            : paragraphs.length === 0 ? <p>暂无正文内容</p>
            : paragraphs.map((p, i) => <p key={i}>{p}</p>)}
        </div>
      </div>

      <div className="reader-bottombar" style={{ display: showToolbar ? 'flex' : 'none' }}>
        <button className="btn btn-ghost" disabled={index <= 0} onClick={() => setIndex(i => i - 1)}>上一章</button>
        <button className="btn btn-ghost" onClick={cacheCurrent}>缓存本章</button>
        <button className="btn btn-ghost" disabled={index >= chapters.length - 1} onClick={() => setIndex(i => i + 1)}>下一章</button>
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
