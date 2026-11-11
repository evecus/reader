import { useEffect, type ReactNode } from 'react';
import { Outlet, useLocation } from 'react-router-dom';
import { useAuthStore } from '../shared/stores/auth.store.ts';
import { useRuntime } from './providers/runtime-context.tsx';
import { LoginPage } from '../features/auth/pages/LoginPage.tsx';
import { AppNav, navMode } from './AppNav.tsx';
import { ThemeController } from '../shared/theme/theme.ts';
import { FloatingNotice } from '../ui/floating-notice.tsx';

export function AppShell() {
  return <AuthBoundary><AuthenticatedShell /></AuthBoundary>;
}

function AuthenticatedShell() {
  const runtime = useRuntime();
  const location = useLocation();
  const mode = navMode(location.pathname);
  // One app theme for every page; the reader may wear its own reading theme on top.
  useEffect(() => {
    const theme = new ThemeController(runtime.settingsApi.preferenceScope());
    return () => theme.dispose();
  }, [runtime]);
  useEffect(() => {
    if (mode === 'hidden') delete document.body.dataset.appNav; else document.body.dataset.appNav = mode;
    return () => { delete document.body.dataset.appNav; };
  }, [mode]);
  return <div className="app-shell">
    <div className="app-shell-body"><Outlet /></div>
    {mode !== 'hidden' && <AppNav mode={mode} />}
  </div>;
}

export function AuthBoundary({ children }: { children?: ReactNode }) {
  const session = useAuthStore(state => state.session);
  if (!session) return <LoginPage />;
  return <>{children}</>;
}

/** 我的 (/me) routes: the page itself. Theme and navigation belong to AuthenticatedShell. */
export function SettingsLayout() { return <Outlet />; }

export function LoadingPage() { return <FloatingNotice message="正在加载…" busy />; }
