import { ApiError, request } from './api';
import type { Attachment, Draft } from './contracts';

/** Serialize writes so delayed autosaves cannot overwrite a newer draft. */
export class DraftWriter {
  private tail: Promise<void> = Promise.resolve();
  private revision: number;
  private failure: unknown;
  constructor(private sessionId: string, initial: Draft) { this.revision = initial.revision; }
  save(text: string, attachment: Attachment | null): Promise<void> {
    const next = this.tail.then(async () => {
      if (this.failure) throw this.failure;
      const path = `/api/workspace/sessions/${encodeURIComponent(this.sessionId)}/draft`;
      let saved: Draft;
      try {
        saved = await request<Draft>(path, { method: 'PUT', body: JSON.stringify({ revision: this.revision, text, attachment }) });
      } catch (error) {
        // A lost HTTP response can hide a successful commit. Confirm exact
        // contents before retrying; never silently overwrite another window.
        const current = await request<Draft>(path).catch(() => null);
        if (current && current.text === text && JSON.stringify(current.attachment) === JSON.stringify(attachment)) saved = current;
        else throw error;
      }
      this.revision = saved.revision;
    });
    this.tail = next.catch((error: unknown) => { if (error instanceof ApiError && error.status === 409) this.failure = error; });
    return next;
  }
  flush(): Promise<void> { return this.tail.then(() => { if (this.failure) throw this.failure; }); }
}
