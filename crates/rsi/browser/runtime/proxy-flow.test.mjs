import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import net from 'node:net';
import {once} from 'node:events';
import test from 'node:test';
import {PacketOutput, ProxySocket} from './proxy-flow.mjs';
import {cdpPacket,decodeCdp} from './cdp.mjs';
class Port extends EventEmitter {
  constructor() { super(); this.accepted = false; this.writes = []; this.callbacks = []; this.data = Buffer.alloc(0); this.destroyed = false; }
  write(bytes, callback) { this.writes.push(bytes); if (callback) this.callbacks.push(callback); return this.accepted; }
  pause() {}
  resume() {}
  get readableLength() { return this.data.length; }
  read(n) { const value=this.data.subarray(0,n); this.data=this.data.subarray(n); return value; }
  destroy() { this.destroyed=true; this.emit('close'); }
  end() { this.ended=true; }
}
test('public output repairs scalar strings and preserves opaque private CDP',()=>{
  const port=new Port();port.accepted=true;
  const output=new PacketOutput(port,error=>{throw error;});
  output.emit({kind:'session_result',result:{text:'page\ud800'}});
  assert.equal(JSON.parse(port.writes[0]).result.text,'page�');
  port.callbacks.shift()();
  const value={id:1,result:{value:'private\udfff'}};
  output.emit(cdpPacket(value));
  assert.deepEqual(decodeCdp(JSON.parse(port.writes[1]).value),value);
  assert.equal(output.bytes,port.writes[1].length);
  port.callbacks.shift()();assert.equal(output.bytes,0);
});
for (const callbackFirst of [true, false]) test('stdout charge lasts through callback and drain: '+callbackFirst, () => {
  const port=new Port();
  const output=new PacketOutput(port, error=>{throw error;});
  output.emit({kind:'proxy_data',id:1,data:'abc'});
  const charged=output.bytes;
  assert(charged>0); assert(!output.proxyReady);
  if(callbackFirst) { port.callbacks.shift()(); assert.equal(output.bytes,charged); port.emit('drain'); }
  else { port.emit('drain'); assert.equal(output.bytes,charged); port.callbacks.shift()(); }
  assert.equal(output.bytes,0); assert(output.proxyReady);
});
test('proxy share cannot consume reserved control allowance', () => {
  const port=new Port(); const output=new PacketOutput(port,()=>{});
  output.emit({kind:'proxy_data',data:'a'.repeat(4*1024*1024)});
  assert.throws(()=>output.emit({kind:'proxy_data',data:'b'.repeat(4*1024*1024)}), /budget/);
  output.emit({kind:'cdp',value:'c'.repeat(4*1024*1024)});
  assert(output.bytes<=16*1024*1024); assert(output.proxyBytes<=8*1024*1024);
});
test('CONNECT head and socket reads share two FIFO credits', () => {
  const port=new Port(); const packets=[];
  const output={proxyReady:true,emit:packet=>packets.push(packet)};
  port.data=Buffer.alloc(3*32768);
  const socket=new ProxySocket(port,1,Buffer.from('head'),output,()=>{});
  socket.open();
  assert.equal(packets.length,2); assert.deepEqual(socket.pending,[4,32768]);
  assert.equal(port.readableLength,2*32768);
  socket.acknowledge(4); assert.equal(packets.length,3);
  assert.throws(()=>socket.acknowledge(4), /receipt/);
  socket.destroy();
});
for (const callbackFirst of [true,false]) test('local proxy writes acknowledge only after actual receipt: '+callbackFirst, () => {
  const port=new Port(); const packets=[];
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:packet=>packets.push(packet)},()=>{});
  socket.receive(Buffer.from('first').toString('base64'));
  socket.receive(Buffer.from('second').toString('base64'));
  assert.throws(()=>socket.receive('eA=='), /credit/);
  if(callbackFirst) { port.callbacks.shift()(); assert.equal(packets.length,0); port.emit('drain'); }
  else { port.emit('drain'); assert.equal(packets.length,0); port.callbacks.shift()(); }
  assert.deepEqual(packets,[{kind:'proxy_ack',id:1,bytes:5}]);
  assert.equal(port.writes.length,2);
  socket.destroy();
});
test('stdout backpressure pauses proxy reads until output drains', () => {
  const port=new Port(); const packets=[]; const output={proxyReady:false,emit:packet=>packets.push(packet)};
  port.data=Buffer.from('waiting');
  const socket=new ProxySocket(port,1,Buffer.alloc(0),output,()=>{});
  socket.open(); assert.equal(packets.length,0);
  output.proxyReady=true; socket.pump(); assert.equal(packets.length,1);
  socket.destroy();
});
test('closing a stalled socket clears pending writes without acknowledging them', () => {
  const port=new Port(); const packets=[]; let closed=0;
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:packet=>packets.push(packet)},()=>closed++);
  socket.receive('eA=='); socket.destroy(); port.callbacks.shift()(); port.emit('drain');
  assert.equal(closed,1); assert.equal(packets.length,0); assert.equal(socket.current,null);
});
test('destroyed socket retains ownership until close and ignores late receipts', () => {
  const port=new Port(); const packets=[]; let closed=0;
  port.destroy = () => { port.destroyed = true; };
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:packet=>packets.push(packet)},()=>closed++);
  socket.receive('eA=='); socket.destroy();
  port.callbacks.shift()(); port.emit('drain'); socket.receive('eA==');
  assert.equal(closed,0); assert.equal(packets.length,0); assert.equal(port.writes.length,1);
  port.emit('close'); assert.equal(closed,1); assert.equal(socket.current,null);
});

for (const callbackFirst of [true, false]) test('graceful close flushes both admitted writes and retains native ownership: '+callbackFirst, () => {
  const port=new Port(); const packets=[]; let closed=0;
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:packet=>packets.push(packet)},()=>closed++);
  try {
    socket.receive('Zmlyc3Q='); socket.receive('c2Vjb25k'); socket.close();
    assert.equal(port.destroyed,false); assert.equal(port.ended,undefined);
    for (let i=0;i<2;i++) {
      if(callbackFirst) { port.callbacks.shift()(); assert.equal(packets.length,i); port.emit('drain'); }
      else { port.emit('drain'); assert.equal(packets.length,i); port.callbacks.shift()(); }
      assert.equal(packets.length,i+1);
    }
    assert.equal(port.ended,true); assert.equal(closed,0);
    port.emit('close'); assert.equal(closed,1);
  } finally { socket.destroy(); }
});

test('graceful close preserves corked final bytes over a real TCP socket', {timeout:3000}, async t => {
  const server=net.createServer({highWaterMark:1024}); server.listen(0,'127.0.0.1'); await once(server,'listening');
  const accepted=once(server,'connection');
  const client=net.connect(server.address().port,'127.0.0.1');
  const connected=once(client,'connect');
  const chunks=[]; client.on('data',chunk=>chunks.push(chunk));
  const eof=once(client,'close');
  const [port]=await accepted; const retired=once(port,'close');
  await connected;
  const expected=Buffer.alloc(2*32768,0x61);
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:()=>{}},()=>{});
  t.after(()=>{socket.destroy(); client.destroy(); server.close();});
  try {
    port.cork();
    socket.receive(expected.subarray(0,32768).toString('base64'));
    assert.equal(socket.current.drained,false);
    socket.receive(expected.subarray(32768).toString('base64'));
    socket.close(); port.uncork();
    await eof; await retired;
    assert.deepEqual(Buffer.concat(chunks),expected);
  } finally {
    socket.destroy(); client.destroy(); await new Promise(resolve=>server.close(resolve));
  }
});

for(const encoded of ['Zg','Zh==','Zg==\n','_w==','']) test('noncanonical base64 rejects before any socket write: '+JSON.stringify(encoded), () => {
  const port=new Port();
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:()=>{}},()=>{});
  assert.throws(()=>socket.receive(encoded), /encoding/);
  assert.equal(port.writes.length,0); assert.equal(socket.receiving.length,0);
  assert.equal(socket.current,null); socket.destroy();
});

test('graceful close has one bounded deadline even when the peer never closes', t => {
  t.mock.timers.enable({apis:['setTimeout']});
  const port=new Port(); let closed=0;
  const socket=new ProxySocket(port,1,Buffer.alloc(0),{proxyReady:true,emit:()=>{}},()=>closed++);
  socket.close(); socket.close();
  assert.equal(port.ended,true); assert.equal(closed,0);
  t.mock.timers.tick(4999); assert.equal(port.destroyed,false);
  t.mock.timers.tick(1); assert.equal(port.destroyed,true); assert.equal(closed,1);
});

test('failed output emission cannot create phantom byte credits', () => {
  const port=new Port();
  const socket=new ProxySocket(port,1,Buffer.from('head'),{proxyReady:true,emit:()=>{throw new Error('injected output failure');}},()=>{});
  assert.throws(()=>socket.open(), /injected output failure/);
  assert.deepEqual(socket.pending,[]); socket.destroy();
});
