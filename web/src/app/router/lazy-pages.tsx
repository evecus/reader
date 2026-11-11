import { lazy, Suspense, type ComponentType, type LazyExoticComponent } from 'react';
import { FloatingNotice } from '../../ui/floating-notice.tsx';

/**
 * Screens most sessions never open, loaded on first visit. The reader
 * (EPUB/PDF/ZIP parsing, pagination, TTS) and the source editors were half of
 * the first load for someone who only opens the shelf. Each wrapper keeps the
 * page's own name so the route tree still reads as `ReaderPage`, …
 * rather than as anonymous lazy objects.
 */
function deferred<M, K extends keyof M>(load: () => Promise<M>, name: K): LazyExoticComponent<ComponentType> {
  return lazy(() => load().then(module => ({ default: module[name] as ComponentType })));
}

const fallback = <FloatingNotice message="正在加载…" busy />;

const reader = () => import('../../features/reader/pages/ReaderPage.tsx');
const sourceEditor = () => import('../../features/sources/pages/SourceEditorPage.tsx');
const sourceCredentials = () => import('../../features/sources/pages/SourceCredentialsPage.tsx');
const sourceExtension = () => import('../../features/sources/pages/SourceExtensionPage.tsx');

const Reader = deferred(reader, 'ReaderPage');
const SourceEditor = deferred(sourceEditor, 'SourceEditorPage');
const SourceCredentials = deferred(sourceCredentials, 'SourceCredentialsPage');
const SourceExtension = deferred(sourceExtension, 'SourceExtensionPage');

export function ReaderPage() { return <Suspense fallback={fallback}><Reader /></Suspense>; }
export function SourceEditorPage() { return <Suspense fallback={fallback}><SourceEditor /></Suspense>; }
export function SourceCredentialsPage() { return <Suspense fallback={fallback}><SourceCredentials /></Suspense>; }
export function SourceExtensionPage() { return <Suspense fallback={fallback}><SourceExtension /></Suspense>; }
