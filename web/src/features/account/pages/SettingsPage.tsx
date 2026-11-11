import { Navigate, useNavigate, useParams } from 'react-router-dom';
import { useRuntime } from '../../../app/providers/runtime-context.tsx';
import { Icon } from '../../../ui/toolkit.tsx';
import { AccountSettings, SETTINGS_TITLES, type SettingsPanel } from '../components/settings.tsx';

function isPanel(value: string | undefined): value is SettingsPanel {
  return (Object.keys(SETTINGS_TITLES) as string[]).includes(value ?? '');
}

/**
 * One settings panel of 我的, at /me/:panel. Back returns to 我的; an unknown
 * panel does the same.
 */
export function SettingsPage() {
  const runtime = useRuntime();
  const navigate = useNavigate();
  const { panel } = useParams();
  if (!isPanel(panel)) return <Navigate to="/me" replace />;

  return <div className="media-screen media-page">
    <header className="media-heading">
      <div className="media-page-heading">
        <button type="button" className="media-back-button" aria-label="← 返回我的" title="返回我的" onClick={() => navigate('/me')}><Icon name="chevron-left" /></button>
        <h1>{SETTINGS_TITLES[panel]}</h1>
      </div>
    </header>
    <AccountSettings
      scope={runtime.settingsApi.preferenceScope()}
      account={runtime.settingsApi.accountInfo()}
      panel={panel}
      onBack={() => navigate('/me')}
      onReaderSettings={patch => { void runtime.updateSettings(patch); }}
    />
  </div>;
}
