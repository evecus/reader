import { useEffect, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { useToast, PageHeader } from '../App'
import { IconCheck, IconClose, IconLink, IconPlus, IconTrash, IconUpload } from '../icons'
import type { BookSource } from '../types'

type ImportTab = 'file' | 'url'

function readFile(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(String(reader.result ?? ''))
    reader.onerror = () => reject(new Error('文件读取失败'))
    reader.readAsText(f, 'utf-8')
  })
}

export default function SourcesPage() {
  const [sources, setSources] = useState<BookSource[]>([])
  const [loading, setLoading] = useState(true)
  const [showImport, setShowImport] = useState(false)
  const [tab, setTab] = useState<ImportTab>('file')
  const [file, setFile] = useState<File | null>(null)
  const [url, setUrl] = useState('')
  const [dragging, setDragging] = useState(false)
  const [importing, setImporting] = useState(false)
  const [testing, setTesting] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const navigate = useNavigate()
  const toast = useToast()

  const load = () => {
    api.listSources().then(setSources).catch(e => toast(e.message)).finally(() => setLoading(false))
  }
  useEffect(load, [])

  const closeImport = () => {
    setShowImport(false)
    setFile(null)
    setUrl('')
    setDragging(false)
  }

  const doImport = async () => {
    let content: string
    if (tab === 'url') {
      const link = url.trim()
      if (!link) { toast('请输入书源订阅链接'); return }
      if (!/^https?:\/\//i.test(link)) { toast('链接需以 http:// 或 https:// 开头'); return }
      content = link
    } else {
      if (!file) { toast('请选择书源 JSON 文件'); return }
      if (file.size > 20 * 1024 * 1024) { toast('文件过大，请选择 20MB 以内的 JSON 文件'); return }
      try {
        content = await readFile(file)
      } catch (e) { toast((e as Error).message); return }
      if (!content.trim()) { toast('文件内容为空'); return }
    }
    setImporting(true)
    try {
      const r = await api.importSources(content)
      toast(`成功导入 ${r.imported} 个书源`)
      closeImport()
      load()
    } catch (e) { toast((e as Error).message) }
    finally { setImporting(false) }
  }

  const toggle = async (s: BookSource) => {
    try { await api.toggleSource(s.bookSourceUrl, !s.enabled); load() }
    catch (e) { toast((e as Error).message) }
  }

  const remove = async (s: BookSource) => {
    if (!confirm(`确定删除书源「${s.bookSourceName}」？`)) return
    try { await api.deleteSource(s.bookSourceUrl); toast('已删除'); load() }
    catch (e) { toast((e as Error).message) }
  }

  const test = async (s: BookSource) => {
    setTesting(s.bookSourceUrl)
    try {
      const r = await api.testSource(s.bookSourceUrl)
      toast(r.ok ? `测试成功，找到 ${r.count} 本` : `测试失败: ${r.error}`)
    } catch (e) { toast((e as Error).message) }
    finally { setTesting(null) }
  }

  const enabledCount = sources.filter(s => s.enabled).length

  return (
    <>
      <PageHeader
        title="书源管理"
        subtitle={loading ? '正在加载书源…' : `共 ${sources.length} 个书源，已启用 ${enabledCount} 个`}
        actions={<button className="btn btn-primary" onClick={() => setShowImport(true)}><IconPlus /> 导入书源</button>}
      />
      {loading ? <div className="loading"><div className="spinner" />加载中…</div>
        : sources.length === 0 ? (
          <div className="empty">
            <div className="empty-title">还没有书源</div>
            <p>可导入本地书源 JSON 文件，或远程订阅链接</p>
            <button className="btn btn-primary" onClick={() => setShowImport(true)}><IconPlus /> 导入书源</button>
          </div>
        ) : (
          <div className="source-grid">
            {sources.map(s => (
              <div className="source-card" key={s.bookSourceUrl}>
                <div className="row">
                  <span className="name" title={s.bookSourceName}>{s.bookSourceName}</span>
                  {s.bookSourceGroup && <span className="group">{s.bookSourceGroup}</span>}
                  <div className={`switch ${s.enabled ? 'on' : ''}`} onClick={() => toggle(s)} title={s.enabled ? '已启用' : '已停用'} />
                </div>
                <div className="url" title={s.bookSourceUrl}>{s.bookSourceUrl}</div>
                <div className="source-actions">
                  <button className="btn btn-ghost" onClick={() => test(s)} disabled={testing === s.bookSourceUrl}>
                    {testing === s.bookSourceUrl ? '测试中…' : '测试'}
                  </button>
                  <button className="btn btn-ghost" onClick={() => navigate(`/explore?origin=${encodeURIComponent(s.bookSourceUrl)}`)}>发现</button>
                  <button className="btn btn-ghost" onClick={() => remove(s)}><IconTrash /> 删除</button>
                </div>
              </div>
            ))}
          </div>
        )}

      {showImport && (
        <div className="dialog-mask" onClick={closeImport}>
          <div className="dialog" onClick={e => e.stopPropagation()}>
            <div className="dialog-head">
              <h2>导入书源</h2>
              <button className="icon-btn-sm" onClick={closeImport}><IconClose /></button>
            </div>

            <div className="import-tabs">
              <button className={`import-tab ${tab === 'file' ? 'active' : ''}`} onClick={() => setTab('file')}>
                <IconUpload /> 本地文件
              </button>
              <button className={`import-tab ${tab === 'url' ? 'active' : ''}`} onClick={() => setTab('url')}>
                <IconLink /> 远程链接
              </button>
            </div>

            {tab === 'file' ? (
              <>
                <input
                  ref={fileInput}
                  type="file"
                  accept=".json,.txt,application/json,text/plain"
                  hidden
                  onChange={e => setFile(e.target.files?.[0] ?? null)}
                />
                <div
                  className={`file-drop ${dragging ? 'dragging' : ''}`}
                  onClick={() => fileInput.current?.click()}
                  onDragOver={e => { e.preventDefault(); setDragging(true) }}
                  onDragLeave={() => setDragging(false)}
                  onDrop={e => {
                    e.preventDefault()
                    setDragging(false)
                    const f = e.dataTransfer.files?.[0]
                    if (f) setFile(f)
                  }}
                >
                  <IconUpload className="file-drop-icon" />
                  {file ? (
                    <>
                      <div className="file-name">{file.name}</div>
                      <div className="file-hint">{(file.size / 1024).toFixed(1)} KB · 点击可重新选择</div>
                    </>
                  ) : (
                    <>
                      <div className="file-name">点击选择，或拖入书源文件</div>
                      <div className="file-hint">支持 .json / .txt，单个书源或书源数组</div>
                    </>
                  )}
                </div>
              </>
            ) : (
              <>
                <p className="import-tip">输入书源订阅链接（返回单个对象或数组）</p>
                <input
                  className="import-url"
                  type="url"
                  value={url}
                  onChange={e => setUrl(e.target.value)}
                  placeholder="https://example.com/shuyuan.json"
                />
                <p className="import-note">将从该地址在线获取书源内容并导入</p>
              </>
            )}

            <div className="dialog-actions">
              <button className="btn btn-ghost" onClick={closeImport}>取消</button>
              <button className="btn btn-primary" onClick={doImport} disabled={importing}>
                <IconCheck /> {importing ? '导入中…' : '确定导入'}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  )
}
