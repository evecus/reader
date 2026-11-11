// @vitest-environment jsdom
import { act } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render } from '../src/shared/ui/render-root.ts';
import { RuntimeContext } from '../src/app/providers/runtime-context.tsx';
import { useAuthStore } from '../src/shared/stores/auth.store.ts';
import { HomePage, greeting } from '../src/features/home/pages/HomePage.tsx';

const root = document.createElement('div');
document.body.append(root);
const user = { id: 'u1', username: 'lin', displayName: '林间', role: 'member', createdAt: 0 };
const book = { id: 'b1', title: '夜航船', author: '示例', format: 'epub', coverUrl: null, percentage: 0.42, chapterTitle: '第十二章 夜雨', lastReadAt: 1 };

function mount(reading: object[] = [book]) {
  const runtime = {
    api: {
      baseUrl: 'https://reader.example.com',
      currentSession: () => ({ user }),
      continueReading: vi.fn().mockResolvedValue(reading),
      listBooks: vi.fn().mockResolvedValue({ items: [book], total: 1 }),
      coverBytes: vi.fn().mockRejectedValue(new Error('none')),
    },
    settingsApi: { preferenceScope: () => 'home-test' },
  } as any;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  act(() => render(
    <RuntimeContext.Provider value={runtime}><QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/home']}><Routes><Route path="/home" element={<HomePage />} /></Routes></MemoryRouter>
    </QueryClientProvider></RuntimeContext.Provider>,
    root,
  ));
}
const settle = () => act(async () => { await new Promise(resolve => setTimeout(resolve, 20)); });

afterEach(() => { act(() => render(null, root)); useAuthStore.setState({ session: null, verifiedUser: null }); });

describe('首页', () => {
  it('greets by time of day', () => {
    expect([2, 8, 14, 21].map(greeting)).toEqual(['夜深了', '早上好', '下午好', '晚上好']);
  });

  it('resumes the book, then shows the shelf, with no media cards', async () => {
    useAuthStore.setState({ verifiedUser: user as any });
    mount();
    await settle();
    expect(root.querySelector('h1')?.textContent).toContain('林间');
    const hero = root.querySelector<HTMLAnchorElement>('a.home-hero')!;
    expect(hero.getAttribute('href')).toBe('/book/b1');
    expect(hero.textContent).toContain('第十二章 夜雨');
    expect(hero.textContent).toContain('42%');
    expect(root.querySelector('a.home-card')).toBeNull();
    expect(root.textContent).not.toContain('/media/');
    expect(root.querySelectorAll('.home-shelf .book-card').length).toBe(1);
  });

  it('invites the reader to the shelf when nothing is in progress', async () => {
    useAuthStore.setState({ verifiedUser: user as any });
    mount([]);
    await settle();
    expect(root.querySelector('a.home-hero')).toBeNull();
    expect(root.querySelector('.home-empty a')?.getAttribute('href')).toBe('/shelf');
  });
});
