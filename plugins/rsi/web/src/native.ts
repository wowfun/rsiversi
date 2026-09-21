// The document keeps one transport contract. Native execution remains in rsi-gui;
// this adapter contains no Session, provider, or credential policy.
export class NativeDocument {
  onmessage?: (event: { data: unknown }) => void;
  onerror?: (event: { preventDefault(): void }) => void;
  private closed = false;
  private pumping = false;
  private base?: string;
  private waiting?: { id: string; resolve(value: { resync: boolean; renderer?: unknown }): void };
  private abort = new AbortController();
  private async request(path: string, body?: BodyInit): Promise<Response> {
    const response = await fetch(path, { method: body === undefined ? 'GET' : 'POST', body, signal: this.abort.signal });
    if (!response.ok) {
      const source = await response.text();
      let failure;
      try { failure = JSON.parse(source); } catch { /* Unknown outcomes do not authorize replay. */ }
      const known = failure && typeof failure.message === 'string' &&
        ((failure.code === 'busy' && failure.notAdmitted === true && failure.retryable === true) ||
         (['closed', 'invalid'].includes(failure.code) && failure.notAdmitted === true && failure.retryable === false) ||
         (failure.code === 'failed' && failure.notAdmitted === false && failure.retryable === false));
      throw Object.assign(new Error(known ? failure.message : 'Native request failed'), {
        notAdmitted: known && failure.notAdmitted === true,
        retryable: known && failure.retryable === true,
      });
    }
    return response;
  }
  postMessage(data: any): void {
    if (this.closed) throw new Error('Native document is closed');
    if (data.kind === 'ack') {
      const waiting = this.waiting;
      if (waiting && waiting.id === data.frame_id) waiting.resolve({ resync: data.resync === true, renderer: data.renderer });
      return;
    }
    if (data.kind !== 'call') return;
    void (async () => {
      try {
        let result: unknown;
        if (data.method === 'disconnect') {
          result = await (await this.request('/_disconnect', '')).json();
        } else {
          let body: BodyInit;
          if (data.method === 'import_image') {
            const { pane, generation, bytes } = data.payload;
            body = new Blob([JSON.stringify({ pane, generation }) + '\n', bytes]);
          } else body = typeof data.payload === 'string' ? data.payload : JSON.stringify(data.payload ?? null);
          const response = await this.request(`/_call/${data.method}`, body);
          result = ['read_image', 'ui_source'].includes(data.method) ? new Uint8Array(await response.arrayBuffer()) : await response.text();
        }
        this.onmessage?.({ data: { kind: 'reply', id: data.id, result } });
        if (data.method === 'connect' && !this.pumping) {
          this.pumping = true;
          void this.views().catch(error => {
            if (!this.closed) this.onmessage?.({ data: { kind: 'failed', error: String(error) } });
          });
        }
      } catch (error) {
        if (!this.closed) this.onmessage?.({ data: { kind: 'reply', id: data.id, error: String(error),
          notAdmitted: error instanceof Error && (error as any).notAdmitted === true,
          retryable: error instanceof Error && (error as any).retryable === true } });
      }
    })();
  }
  private async views(): Promise<void> {
    while (!this.closed) {
      const frame = await (await this.request(`/_frame?${this.base ?? ''}`)).json();
      const settled = await new Promise<{ resync: boolean; renderer?: unknown }>(resolve => {
        this.waiting = { id: frame.view.frame_id, resolve };
        this.onmessage?.({ data: { kind: 'view', view: frame.view, assets: frame.assets } });
      });
      this.waiting = undefined;
      if (this.closed) return;
      const renderer = settled.renderer ? { revision: (settled.renderer as any).revision, accept: (settled.renderer as any).accept } : undefined;
      await this.request('/_ack', JSON.stringify({ frame_id: frame.view.frame_id, renderer }));
      this.base = settled.resync ? undefined : frame.view.frame_id;
    }
  }
  terminate(): void {
    if (this.closed) return;
    this.closed = true; this.waiting?.resolve({ resync: true }); this.abort.abort();
    void fetch('/_failed', { method: 'POST', body: '' }).catch(() => {});
  }
}
