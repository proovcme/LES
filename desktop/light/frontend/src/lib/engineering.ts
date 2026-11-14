import { request } from './api';
export interface ModelLocator {
  provider: 'uvyazka'; project_id: string; dataset_id: string; object_id: string; native_id: string;
  source_sha256: string; source_name: string; section: string;
  coverage: { status: string; limitations?: string[] }; approval_status: 'unreviewed';
}
const key = 'les.uvyazka.url.v1';
export function companionAddress(): string { try { return localStorage.getItem(key) || 'http://127.0.0.1:8772/'; } catch { return 'http://127.0.0.1:8772/'; } }
export function rememberCompanion(url: string) { try { localStorage.setItem(key, url); } catch { /* Address can be re-entered; source data is not stored here. */ } }
export function engineering<T>(url: string, action: string, args: object = {}): Promise<T> {
  return request<T>('/api/integrations/uvyazka', { method: 'POST', body: JSON.stringify({ url, action, ...args }), signal: AbortSignal.timeout(185000) });
}
