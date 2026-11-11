import type { AppSettings } from '../../../store/settings.ts';
import { Icon } from '../../../ui/toolkit.tsx';
import type { AccountInfo } from '../../../api/settings.ts';
import { ThemeSettings } from './theme-settings.tsx';

export type SettingsPanel = 'theme' | 'account';

export const SETTINGS_TITLES: Record<SettingsPanel, string> = {
  theme: '外观与主题',
  account: '账号与连接',
};

/**
 * One settings panel of 我的 (/me/:panel). 我的 is the list, and each row opens
 * one of these panels. Only 外观与主题 and 账号与连接 remain — this client reads.
 */
export function AccountSettings({ scope, account, onBack, panel, onReaderSettings, backLabel = '返回我的' }: {
  scope: string;
  account?: AccountInfo | null;
  onBack: () => void;
  panel: SettingsPanel;
  onReaderSettings?: (patch: Partial<AppSettings>) => void;
  backLabel?: string;
}) {
  return <section className="media-settings" aria-label={SETTINGS_TITLES[panel]}>
    <div className="media-settings-heading">
      <button type="button" className="media-back-button" data-media-back aria-label={backLabel} title={backLabel} onClick={onBack}><Icon name="chevron-left" /></button>
      <h1>{SETTINGS_TITLES[panel]}</h1>
    </div>
    {panel === 'theme' && <ThemeSettings scope={scope} {...(onReaderSettings ? { onReaderSettings } : {})} />}
    {panel === 'account' && account && <section className="media-account-panel">
      <div className="media-account-avatar" aria-hidden="true">{Array.from(account.displayName || account.username)[0]}</div>
      <h2>{account.displayName || account.username}</h2>
      <p>@{account.username} · {account.role === 'admin' ? '管理员' : '普通用户'}</p>
      <p className="media-manager-note">书架、阅读进度和笔记按账号隔离</p>
      <dl><dt>当前服务器</dt><dd>{account.server || window.location.origin}</dd></dl>
    </section>}
  </section>;
}
