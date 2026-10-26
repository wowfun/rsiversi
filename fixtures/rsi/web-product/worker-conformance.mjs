// Runs against the published JavaScript Worker and its actual Rust/Wasm module.
// No mock Worker, codec implementation, or extra production export is involved.
export async function workerConformance({ buildFamily, receipt }) {
  const worker = new Worker('/worker.js', { type: 'module' });
  const pending = new Map();
  let next = 0, frames = 0, firstFrame, rejectFrame;
  const frame = new Promise((resolve, reject) => { firstFrame = resolve; rejectFrame = reject; });
  frame.catch(() => {});
  const timer = setTimeout(() => {
    for (const waiter of pending.values()) waiter.reject(new Error('Worker probe timed out'));
    rejectFrame(new Error('Worker frame timed out'));
    worker.terminate();
  }, 30_000);
  worker.onmessage = ({ data }) => {
    if (data.kind === 'view') {
      const value = JSON.parse(data.view);
      if (!['snapshot', 'patch'].includes(value.kind) || !/^[1-9][0-9]*$/.test(value.frame_id)) {
        rejectFrame(new Error('Invalid Rust presentation frame'));
        return;
      }
      frames++;
      worker.postMessage({ kind: 'ack', frame_id: value.frame_id });
      firstFrame(value);
    } else if (data.kind === 'reply') {
      const waiter = pending.get(data.id);
      if (!waiter) return;
      pending.delete(data.id);
      if (data.error) waiter.reject(Object.assign(new Error(data.error), {notAdmitted:data.notAdmitted === true})); else waiter.resolve(data.result);
    }
  };
  const call = (method, payload) => new Promise((resolve, reject) => {
    const id = ++next; pending.set(id, { resolve, reject });
    worker.postMessage({ kind: 'call', id, method, payload });
  });
  const rejected = [];
  try {
    for(const method of ['resource_open','terminal']){
      let refused=false;
      try{await call(method,'{}')}catch(error){if(!error.notAdmitted)throw new Error(`${method} missing-owner refusal lost admission evidence`);refused=true;}
      if(!refused)throw new Error(`${method} unexpectedly admitted without an application`);
      rejected.push(`${method} before connect`);
    }
    const identity = JSON.parse(await call('connect', { receipt: JSON.stringify(receipt), devHttp: false, buildFamily }));
    if (!identity.endpoint_id || identity.principal?.kind !== 'device') throw new Error('Invalid identity reply');
    await frame;
    await call('command', JSON.stringify({ action: 'refresh' }));
    await call('command', JSON.stringify({ action: 'external_catalog', next: false }));
    for (const [name, method, payload] of [
      ['malformed JSON', 'command', '{'],
      ['unknown action', 'command', '{"action":"not_an_action"}'],
      ['unknown structured command field', 'command', '{"action":"external_catalog","next":false,"extra":true}'],
      ['malformed submission', 'prepare_submission', '{}'],
      ['malformed terminal command', 'terminal', '{}'],
    ]) {
      try { await call(method, payload); } catch (error) {
        if (method === 'command' && !error.notAdmitted) throw new Error(`${name} lost its Rust admission refusal`);
        rejected.push(name); continue;
      }
      throw new Error(`${name} was accepted`);
    }
    await call('command', JSON.stringify({ action: 'refresh' }));
    const resources = await call('disconnect', false);
    if (resources.active_requests !== 0 || resources.pending_timers !== 0 || resources.active_alarms !== 0) {
      throw new Error(`Worker did not drain: ${JSON.stringify(resources)}`);
    }
    return { ok: true, actualRustWasm: true, frames, rejected, resources };
  } finally { clearTimeout(timer); worker.terminate(); }
}
