import { request } from './api';
export interface ModelLocator {
  provider: 'uvyazka'; project_id: string; dataset_id: string; object_id: string; native_id: string;
  source_sha256: string; source_name: string; section: string;
  coverage: { status: string; limitations?: string[] }; approval_status: 'unreviewed';
}
const key = 'les.uvyazka.url.v1';
export function savedCompanion(): string | null { try { return localStorage.getItem(key); } catch { return null; } }
export function companionAddress(): string { return savedCompanion() || 'http://127.0.0.1:17881/'; }
export function rememberCompanion(url: string) { try { localStorage.setItem(key, url); } catch { /* Address can be re-entered; source data is not stored here. */ } }
export function engineering<T>(url: string, action: string, args: object = {}): Promise<T> {
  return request<T>('/api/integrations/uvyazka', { method: 'POST', body: JSON.stringify({ url, action, ...args }), signal: AbortSignal.timeout(185000) });
}
export interface CompanionProject { id: string; local_id: number; name: string; available: boolean }
interface ProjectSync { projects: CompanionProject[]; default_project?: string; url: string | null }
const syncing = new Map<string, Promise<ProjectSync>>();
export function syncCompanion(url = ''): Promise<ProjectSync> {
  const pending = syncing.get(url);
  if (pending) return pending;
  const sync = engineering<ProjectSync>(url, url ? 'sync_projects' : 'refresh_projects').finally(() => syncing.delete(url));
  syncing.set(url, sync);
  return sync;
}
