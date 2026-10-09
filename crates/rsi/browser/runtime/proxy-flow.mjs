// Private bounded transport accounting; charges include queued and in-flight writes.
export class PacketOutput {
  constructor(stream, failed, ready = () => {}) {
    this.stream = stream; this.failed = failed; this.ready = ready;
    this.queue = []; this.current = null; this.bytes = 0; this.proxyBytes = 0;
    this.blocked = false; this.closed = false;
    stream.on('drain', () => {
      this.blocked = false;
      if (this.current) this.current.drained = true;
      this.finish(); this.ready();
    });
    stream.on('error', error => this.fail(error));
  }
  get proxyReady() { return !this.closed && !this.blocked && this.proxyBytes + 44000 < 8 * 1024 * 1024 && this.bytes + 44000 < 16 * 1024 * 1024; }
  emit(packet) {
    if (this.closed) throw new Error('helper output retired');
    const line = JSON.stringify(packet,(_key,value)=>typeof value==='string'?value.toWellFormed():value);
    if (Buffer.byteLength(line) > 8 * 1024 * 1024) throw new Error('helper frame exceeds bound');
    const bytes = Buffer.from(line + '\n');
    const proxy = packet.kind.startsWith('proxy_');
    if (this.bytes + bytes.length > 16 * 1024 * 1024 || (proxy && this.proxyBytes + bytes.length > 8 * 1024 * 1024)) throw new Error('helper output budget exceeded');
    this.bytes += bytes.length;
    if (proxy) this.proxyBytes += bytes.length;
    this.queue.push({bytes, proxy, callback: false, drained: false, returned: false});
    this.pump();
  }
  pump() {
    if (this.closed || this.current || !this.queue.length) return;
    const item = this.current = this.queue.shift();
    const accepted = this.stream.write(item.bytes, error => {
      if (error) { this.fail(error); return; }
      item.callback = true; this.finish();
    });
    item.drained = accepted;
    item.returned = true;
    this.blocked = !accepted;
    this.finish();
  }
  finish() {
    const item = this.current;
    if (!item || !item.returned || !item.callback || !item.drained || this.closed) return;
    this.bytes -= item.bytes.length;
    if (item.proxy) this.proxyBytes -= item.bytes.length;
    this.current = null;
    this.pump(); this.ready();
  }
  fail(error) {
    if (this.closed) return;
    this.closed = true; this.queue = []; this.current = null;
    this.bytes = 0; this.proxyBytes = 0; this.failed(error);
  }
}
// Peer: ../src/runtime.rs::{ProxyOwner, ProxyEvent}; wire bounds: ../README.md.
// pending mirrors Rust inbound until Written returns proxy_ack. receiving/current
// mirrors Rust outbound until callback+drain emits our proxy_ack. PacketOutput
// separately retains the encoded stdout charge through its own callback+drain.
export class ProxySocket {
  constructor(socket, id, head, output, closed, connect = true) {
    this.socket = socket; this.id = id; this.head = head; this.output = output;
    this.closed = closed; this.pending = []; this.receiving = [];
    this.connect = connect;
    this.current = null; this.opened = false; this.retired = false;
    this.closing = false; this.ended = false;
    socket.pause();
    this.readable = () => this.pump();
    socket.on('readable', this.readable);
    socket.on('drain', () => {
      if (this.current) this.current.drained = true;
      this.finish();
    });
    socket.on('close', () => this.retire());
    socket.on('error', () => this.destroy());
  }
  open() {
    if (this.retired || this.socket.destroyed || this.closing) return;
    if (this.connect) this.socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
    this.opened = true; this.pump();
  }
  pump() {
    while (!this.retired && !this.socket.destroyed && !this.closing && this.opened && this.pending.length < 2 && this.output.proxyReady) {
      let bytes;
      if (this.head.length) { bytes = this.head; this.head = Buffer.alloc(0); }
      else {
        const available = Math.min(32768, this.socket.readableLength);
        if (!available) break;
        bytes = this.socket.read(available);
        if (!bytes) break;
      }
      this.output.emit({kind:'proxy_data', id:this.id, data:bytes.toString('base64')});
      this.pending.push(bytes.length);
    }
  }
  acknowledge(length) {
    if (this.retired || this.socket.destroyed) return;
    if (!Number.isSafeInteger(length) || length < 1 || this.pending.shift() !== length) throw new Error('proxy receipt mismatch');
    this.pump();
  }
  receive(encoded) {
    if (this.retired || this.socket.destroyed || this.closing) return;
    if (typeof encoded !== 'string' || encoded.length > 44000) throw new Error('proxy chunk exceeds bound');
    const bytes = Buffer.from(encoded, 'base64');
    if (!bytes.length || bytes.length > 32768 || bytes.toString('base64') !== encoded || this.receiving.length + Number(!!this.current) >= 2) throw new Error('proxy credit or encoding mismatch');
    this.receiving.push(bytes); this.write();
  }
  write() {
    if (this.retired || this.socket.destroyed || this.current || !this.receiving.length) return;
    const item = this.current = {bytes:this.receiving.shift(), callback:false, drained:false, returned:false};
    item.timer = setTimeout(() => this.destroy(), 5000);
    const accepted = this.socket.write(item.bytes, error => {
      if (error) { this.destroy(); return; }
      item.callback = true; this.finish();
    });
    item.drained = accepted; item.returned = true;
    this.finish();
  }
  finish() {
    const item = this.current;
    if (!item || !item.returned || !item.callback || !item.drained || this.retired || this.socket.destroyed) return;
    clearTimeout(item.timer);
    this.current = null;
    this.output.emit({kind:'proxy_ack',id:this.id,bytes:item.bytes.length});
    this.write(); this.finishClose();
  }
  close() {
    if (this.retired || this.socket.destroyed || this.closing) return;
    this.closing = true;
    // Consume late input locally so EOF can retire the half-closed socket.
    this.socket.removeListener('readable', this.readable); this.socket.resume();
    this.closeTimer = setTimeout(() => this.destroy(), 5000);
    this.finishClose();
  }
  finishClose() {
    if (!this.closing || this.current || this.receiving.length || this.ended || this.retired || this.socket.destroyed) return;
    this.ended = true; this.socket.end();
  }
  destroy() { this.socket.destroy(); }
  retire() {
    if (this.retired) return;
    this.retired = true;
    clearTimeout(this.closeTimer);
    if (this.current) clearTimeout(this.current.timer);
    this.current = null; this.receiving = []; this.pending = []; this.head = Buffer.alloc(0);
    this.closed();
  }
}
