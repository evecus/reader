import { Link } from 'react-router-dom';
import { useState, type ReactNode } from 'react';
import { ChevronRight, LayoutGrid, LibraryBig, LogOut, Palette, Rss, Settings, UserRound, type LucideIcon } from 'lucide-react';
import { useRuntime } from '../../../app/providers/runtime-context.tsx';
import { useAuthStore } from '../../../shared/stores/auth.store.ts';
import { useSettingsStore } from '../../../shared/stores/settings.store.ts';
import { themeSummary } from '../../../shared/theme/theme.ts';
import { useThemeState } from '../../../shared/theme/use-theme.ts';
import { DENSITY_LABELS, ShelfSettingsSheet } from '../../../ui/shelf-settings.tsx';
import { OpdsAccess } from '../../../ui/opds-access.tsx';
import { SystemSettings } from '../../../ui/system-settings.tsx';
import '../../home/styles/home.css';
import '../styles/mine.css';

function Row({ icon: Icon, label, value, to, onClick, danger = false }: { icon: LucideIcon; label: string; value?: string; to?: string; onClick?: () => void; danger?: boolean }) {
  const body = <>
    <span className="mine-row-icon" aria-hidden="true"><Icon size={18} /></span>
    <span className="mine-row-label">{label}</span>
    {value && <span className="mine-row-value">{value}</span>}
    {!danger && <ChevronRight size={16} aria-hidden="true" className="mine-row-chevron" />}
  </>;
  const className = danger ? 'mine-row is-danger' : 'mine-row';
  return to
    ? <Link className={className} to={to}>{body}</Link>
    : <button type="button" className={className} onClick={onClick}>{body}</button>;
}

function Group({ title, order, children }: { title: string; order: number; children: ReactNode }) {
  return <section className="mine-group" aria-label={title} style={{ order }}><h2>{title}</h2><div className="mine-card">{children}</div></section>;
}

function serverHost(baseUrl: string): string {
  try { return new URL(baseUrl).host; } catch { return baseUrl; }
}

/**
 * 我的: the reading client's settings hub. 内容 points at the library and its
 * sources; 偏好 holds this device's look and shelf; 管理 is the administrator's
 * server settings; 账号 is who is signed in. This client reads only.
 */
export function MinePage() {
  const runtime = useRuntime();
  const user = useAuthStore(state => state.verifiedUser ?? state.session?.user ?? null);
  const settings = useSettingsStore(state => state.settings);
  const admin = user?.role === 'admin';
  const scope = runtime.settingsApi.preferenceScope();
  const themeName = themeSummary(useThemeState(scope).preference);
  const [sheet, setSheet] = useState<'shelf' | 'opds' | 'system' | null>(null);
  const name = user?.displayName || user?.username || '';
  return <>
    <div className="media-screen media-page mine-page">
      <header className="home-header"><h1>我的</h1></header>
      {user && <section className="mine-profile" aria-label="当前账号">
        <div className="mine-avatar" aria-hidden="true">{Array.from(name)[0]}</div>
        <div className="mine-profile-text">
          <strong>{name}<span className="mine-role">{admin ? '管理员' : '成员'}</span></strong>
          <small><span className="mine-dot" aria-hidden="true" />已连接 {serverHost(runtime.api.baseUrl)}</small>
        </div>
      </section>}
      <div className="mine-groups">
        <div className="mine-column">
          <Group title="内容" order={1}>
            <Row icon={LibraryBig} label="书库" to="/library" />
            <Row icon={Rss} label="书源" to="/sources" />
          </Group>
          {admin && <Group title="管理" order={3}>
            <Row icon={Settings} label="系统设置" value="用户、注册、服务配置" onClick={() => setSheet('system')} />
          </Group>}
        </div>
        <div className="mine-column">
          <Group title="偏好" order={2}>
            <Row icon={Palette} label="外观与主题" value={themeName} to="/me/theme" />
            <Row icon={LayoutGrid} label="书架显示" value={DENSITY_LABELS[settings.shelfDensity]} onClick={() => setSheet('shelf')} />
            <Row icon={UserRound} label="账号与连接" to="/me/account" />
          </Group>
          <Group title="账号" order={4}>
            <Row icon={LogOut} label="退出登录" danger onClick={() => { void runtime.api.signOut(); }} />
          </Group>
        </div>
      </div>
    </div>
    {sheet === 'shelf' && <ShelfSettingsSheet settings={settings} onPatch={patch => { void runtime.updateSettings(patch); }} onClose={() => setSheet(null)} onOpenExternalReader={() => setSheet('opds')} />}
    {sheet === 'opds' && <OpdsAccess api={runtime.api} onSignedOut={() => { void runtime.api.signOut(); }} onClose={() => setSheet('shelf')} />}
    {sheet === 'system' && <SystemSettings api={runtime.api} onClose={() => setSheet(null)} />}
  </>;
}
