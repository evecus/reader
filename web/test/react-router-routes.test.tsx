// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { matchRoutes } from 'react-router-dom';
import { routes } from '../src/app/router/routes.tsx';

describe('React Router route tree', () => {
  it.each([
    ['/home', 'HomePage'],
    ['/shelf', 'ShelfPage'],
    ['/search?q=三体', 'SearchPage'],
    ['/library', 'LibraryBrowsePage'],
    ['/library/books', 'LibraryPage'],
    ['/library/files', 'LibraryFilesPage'],
    ['/sources', 'SourcesPage'],
    ['/sources/search', 'SourcesPage'],
    ['/sources/manage/new', 'SourceEditorPage'],
    ['/sources/source-1/browse', 'SourceCatalogPage'],
    ['/sources/source-1/credentials', 'SourceCredentialsPage'],
    ['/sources/plugins', 'SourcesPage'],
    ['/sources/source-1/pages/settings', 'SourceExtensionPage'],
    ['/sources/source-1/library', 'SourceExtensionPage'],
    ['/sources/plugins/plugin-1/pages/settings', 'SourceExtensionPage'],
    ['/book/book-1', 'ReaderPage'],
    ['/me', 'MinePage'],
    ['/me/theme', 'SettingsPage'],
    ['/settings', 'Navigate'],
  ])('matches %s through the declarative route tree', (pathname, componentName) => {
    const matches = matchRoutes(routes, pathname);
    expect(matches?.at(-1)?.route.element?.type?.name).toBe(componentName);
  });

  it('falls back to the shelf for the default route, unknown paths and old media links', () => {
    for (const pathname of ['/', '/unknown', '/media/video', '/media/music/albums', '/media/audiobook/narrators']) {
      const matches = matchRoutes(routes, pathname);
      expect(matches?.at(-1)?.route.element?.type?.name).toBe('Navigate');
    }
  });

  it('has no media layout or media page anywhere in the tree', () => {
    const names = JSON.stringify(routes);
    expect(names).not.toContain('features/media');
    const matches = matchRoutes(routes, '/media/video/items/item-1');
    expect(matches?.some(match => match.route.element?.type?.name?.startsWith('Media'))).toBe(false);
  });

  it('does not route reader pages through a media screen or legacy router', () => {
    const matches = matchRoutes(routes, '/book/book-1');
    expect(matches?.at(-1)?.route.element?.type?.name).toBe('ReaderPage');
    expect(matches?.some(match => match.route.element?.type?.name === 'MediaScreen')).toBe(false);
  });
});
