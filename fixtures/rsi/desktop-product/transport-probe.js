// WebDriver installs this after admission. Observe the real shared Worker;
// no product response, bridge invocation, or runtime bytes are substituted.
(() => {
  const original = Worker.prototype.postMessage;
  const pending = new Map(), requests = new Map(), seen = new WeakSet();
  let worker, next = -1, ready;
  const available = new Promise(resolve => { ready = resolve; });
  const probe = window.fixtureTransport = {
    calls: [], replies: [], frames: [], failures: [], observers: [], holdAck: false, held: null,
    async request(method, payload) {
      await available;
      const id = next--;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => { pending.delete(id); reject(new Error('Fixture Worker reply deadline')); }, 35000);
        pending.set(id, reply => { clearTimeout(timer); resolve(reply); });
        worker.postMessage({kind:'call', id, method, payload});
      });
    },
    async call(method, payload) {
      const reply = await this.request(method, payload);
      if (reply.error) throw Object.assign(new Error(reply.error), reply);
      return typeof reply.result === 'string' ? JSON.parse(reply.result) : reply.result;
    },
    releaseAck() {
      this.holdAck = false;
      if (this.held) { original.call(worker, this.held); this.held = null; }
    },
  };
  Worker.prototype.postMessage = function(data, ...rest) {
    if (!seen.has(this)) {
      seen.add(this); worker = this; ready();
      this.addEventListener('message', ({data:reply}) => {
        if (reply.kind === 'reply') {
          const request = requests.get(reply.id); requests.delete(reply.id);
          probe.replies.push({request, ...reply});
          for (const observe of probe.observers) observe('reply', reply, request);
          const resolve = pending.get(reply.id); pending.delete(reply.id); resolve?.(reply);
        } else if (reply.kind === 'view') {
          const frame = {view: typeof reply.view === 'string' ? JSON.parse(reply.view) : reply.view,
            assets: typeof reply.assets === 'string' ? JSON.parse(reply.assets) : reply.assets};
          probe.frames.push(frame);
          for (const observe of probe.observers) observe('frame', frame);
        } else if (reply.kind === 'failed') probe.failures.push(reply.error);
        for (const list of [probe.replies, probe.frames, probe.failures]) if (list.length > 256) list.shift();
      });
    }
    if (data.kind === 'call') {
      requests.set(data.id, data); probe.calls.push(data);
      if (probe.calls.length > 256) probe.calls.shift();
      for (const observe of probe.observers) observe('call', data);
    }
    if (data.kind === 'ack' && probe.holdAck) { probe.held = data; return; }
    return original.call(this, data, ...rest);
  };
})();
