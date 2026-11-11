import { Navigate, type RouteObject } from 'react-router-dom';
import { AppShell, SettingsLayout } from '../App.tsx';
import { RouteErrorBoundary } from './error-boundary.tsx';
import { ShelfPage } from '../../features/shelf/pages/ShelfPage.tsx';
import { LibraryPage, LibraryBrowsePage, LibraryFilesPage } from '../../features/library/pages/LibraryPage.tsx';
import { SourcesPage } from '../../features/sources/pages/SourcesPage.tsx';
import { MinePage } from '../../features/account/pages/MinePage.tsx';
import { HomePage } from '../../features/home/pages/HomePage.tsx';
import { SearchPage } from '../../features/search/pages/SearchPage.tsx';
import { SettingsPage } from '../../features/account/pages/SettingsPage.tsx';
import { SourceCatalogPage } from '../../features/sources/pages/SourceCatalogPage.tsx';
import {
  ReaderPage, SourceEditorPage, SourceCredentialsPage, SourceExtensionPage,
} from './lazy-pages.tsx';

/**
 * The reading client's complete route tree. Media was removed: there is no media
 * layout, page, or API left. Old media deep links fall back to the shelf instead
 * of rendering an empty shell.
 *
 * `/` is the default destination and points at the shelf; 首页 lives at /home.
 */
export const routes = [{
  path: '/', element: <AppShell />, errorElement: <RouteErrorBoundary />, children: [
    { index: true, element: <Navigate to="/shelf" replace /> },
    { path: 'home', element: <HomePage /> },
    { path: 'shelf', element: <ShelfPage /> },
    { path: 'search', element: <SearchPage /> },
    { path: 'library', element: <LibraryBrowsePage /> },
    { path: 'library/books', element: <LibraryPage /> },
    { path: 'library/files', element: <LibraryFilesPage /> },
    { path: 'sources', element: <SourcesPage /> },
    // The legacy hash route /sources/search opens the complete sources hub;
    // catalog browsing remains available through the source-specific route.
    { path: 'sources/search', element: <SourcesPage /> },
    { path: 'sources/manage/new', element: <SourceEditorPage /> },
    { path: 'sources/manage/:sourceId', element: <SourceEditorPage /> },
    { path: 'sources/:sourceId/browse', element: <SourceCatalogPage /> },
    { path: 'sources/:sourceId/credentials', element: <SourceCredentialsPage /> },
    { path: 'sources/plugins', element: <SourcesPage /> },
    { path: 'sources/:sourceId/pages/:pageId', element: <SourceExtensionPage /> },
    // Legacy source extension links use /sources/:sourceId/:pageId. Keep the
    // old deep-link shape working while the React shell emits the namespaced
    // /pages form for new links.
    { path: 'sources/:sourceId/:pageId', element: <SourceExtensionPage /> },
    { path: 'sources/plugins/:pluginId/pages/:pageId', element: <SourceExtensionPage /> },
    // Legacy deep link; user preferences live in 我的 (/me).
    { path: 'settings', element: <Navigate to="/shelf" replace /> },
    { path: 'book/:bookId', element: <ReaderPage /> },
    { path: 'me', element: <SettingsLayout />, children: [
      { index: true, element: <MinePage /> },
      { path: ':panel', element: <SettingsPage /> },
    ] },
    { path: 'media/*', element: <Navigate to="/shelf" replace /> },
    { path: '*', element: <Navigate to="/shelf" replace /> },
  ],
}] satisfies RouteObject[];
