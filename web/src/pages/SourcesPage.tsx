import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'
import { useToast } from '../App'
import { IconBack, IconCheck, IconClose, IconDownload, IconPlus, IconTrash } from '../icons'
import type { BookSource } from '../types'

export default function SourcesPage() {
  const [sources, setSources] = useState<BookSource[]>([])
  const [loading, setLoading] = useState(true)
  const [showImport, setShowImport] = useState(false)
  const [importText, setImportText] = useState('')
  const [importing, setImporting] = useState(false)
  const [testing, setTesting] = useState<string | null>(null)
  const navigate = useNavigate()
  const toast = useToast()

  const load = () => {
    api.listSources().then(setSources).catch(e => toast(e.message)).finally(() => setLoading(false))
  }
  useEffect(load, [])

  const doImport = async () => {
    const text = importText.trim()
    if (!text) { toast('请输入书源 JSON 或 URL'); return }
    setImporting(true)
    try {
      const r = await api.importSources(text)
      toast(`成功导入 ${r.imported} 个书源`)
      setShowImport(false); setImportText(''); load()
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
      <header className="appbar">
        <button className="icon-btn" onClick={() => navigate('/shelf')}><IconBack /></button>
        <div className="title">书源管理</div>
        <button className="icon-btn" onClick={() => setShowImport(true)} title="导入"><IconPlus /></button>
      </header>
      <main className="page">
        <div className="list-section-title">共 {sources.length} 个书源，已启用 {enabledCount} 个</div>
        {loading ? <div className="loading"><div className="spinner" />加载中…</div>
          : sources.length === 0 ? (
            <div className="empty">
              <div>还没有书源</div>
              <p style={{ fontSize: 13, marginTop: 8 }}>导入 Legado 书源 JSON 或网络地址</p>
              <button className="btn btn-primary" style={{ marginTop: 16 }} onClick={() => setShowImport(true)}>
                <IconPlus /> 导入书源
              </button>
            </div>
          ) : sources.map(s => (
            <div className="source-card" key={s.bookSourceUrl}>
              <div className="row">
                <span className="name">{s.bookSourceName}</span>
                {s.bookSourceGroup && <span className="group">{s.bookSourceGroup}</span>}
                <div className={`switch ${s.enabled ? 'on' : ''}`} onClick={() => toggle(s)} />
              </div>
              <div className="url">{s.bookSourceUrl}</div>
              <div className="source-actions">
                <button className="btn btn-ghost" onClick={() => test(s)} disabled={testing === s.bookSourceUrl}>
                  {testing === s.bookSourceUrl ? '测试中…' : '测试'}
                </button>
                <button className="btn btn-ghost" onClick={() => navigate(`/explore?origin=${encodeURIComponent(s.bookSourceUrl)}`)}>发现</button>
                <button className="btn btn-ghost" onClick={() => remove(s)}><IconTrash /> 删除</button>
              </div>
            </div>
          ))}
      </main>

      {showImport && (
        <div className="dialog-mask" onClick={() => setShowImport(false)}>
          <div className="dialog" onClick={e => e.stopPropagation()}>
            <div style={{ display: 'flex', alignItems: 'center', marginBottom: 14 }}>
              <h2 style={{ flex: 1, margin: 0 }}>导入书源</h2>
              <button className="icon-btn-sm" onClick={() => setShowImport(false)}><IconClose /></button>
            </div>
            <p style={{ fontSize: 13, color: 'var(--text-secondary)', marginBottom: 10 }}>
              粘贴 Legado 书源 JSON（单个对象或数组），或书源订阅 URL
            </p>
            <textarea value={importText} onChange={e => setImportText(e.target.value)}
              placeholder={'[{"bookSourceName":"...","bookSourceUrl":"...",...}]'} />
            <div className="dialog-actions">
              <button className="btn btn-ghost" onClick={() => setShowImport(false)}>取消</button>
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
