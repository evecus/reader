import type { ReaderApi } from './client.ts';

/** The account shown on 我的 and its account panel. */
export interface AccountInfo { username: string; displayName: string; role: 'admin' | 'member'; server: string }

/**
 * Account-wide settings access shared by 我的 and its panels: the per-device
 * preference scope (server + user), who is signed in, and the admin/business
 * endpoints behind 系统设置, TTS preview and AI model listing (reading features).
 */
export class SettingsApi {
  constructor(private readonly reader: ReaderApi) {}
  businessSettingsRequest<T>(path = '', method = 'GET', body?: unknown, signal?: AbortSignal) { return this.reader.businessSettingsRequest<T>(path, method, body, signal); }
  businessTtsPreview(values: Record<string, unknown>, voice: string) { return this.reader.businessTtsPreview(values, voice); }
  businessAiModels(values?: { baseUrl?: string; apiKey?: string }) { return this.reader.businessAiModels(values); }
  preferenceScope() { return JSON.stringify([this.reader.baseUrl, this.reader.currentSession()?.user.id ?? 'anonymous']); }
  accountInfo(): AccountInfo | null {
    const user = this.reader.currentSession()?.user;
    return user ? { username: user.username, displayName: user.displayName, role: user.role, server: this.reader.baseUrl } : null;
  }
}
