import assert from 'node:assert/strict';
import {once} from 'node:events';
import net from 'node:net';
import test from 'node:test';
import {createProxy,HeaderAccumulator} from './http-proxy.mjs';

test('fragmented proxy headers retain exact binary payload and a split delimiter at the limit',()=>{
  const header=Buffer.alloc(16384,65),payload=Buffer.from([0,255,13,10]);
  const input=new HeaderAccumulator();let result;
  for(const byte of header)assert.equal(input.push(Buffer.from([byte])),null);
  for(const byte of Buffer.from('\r\n\r'))assert.equal(input.push(Buffer.from([byte])),null);
  result=input.push(Buffer.concat([Buffer.from('\n'),payload]));
  assert.equal(result.end,16384);assert.deepEqual(result.bytes.subarray(result.end+4),payload);
});
test('oversized header and initial payload fail before admission without retaining the chunk',()=>{
  assert.throws(()=>new HeaderAccumulator().push(Buffer.alloc(16388,65)),/header exceeds bound/);
  const input=new HeaderAccumulator();assert.throws(()=>input.push(Buffer.alloc(1000000)),/initial bytes exceed bound/);assert.equal(input.length,0);
  assert.throws(()=>new HeaderAccumulator().push(Buffer.concat([Buffer.from('\r\n\r\n'),Buffer.alloc(32769)])),/initial bytes exceed bound/);
  assert.throws(()=>new HeaderAccumulator(32768).push(Buffer.alloc(32769)),/initial bytes exceed bound/);
});

async function fixture(t,local=null){
  let deliver;const admitted=new Promise(resolve=>deliver=resolve);
  const sockets=new Set();const server=createProxy(packet=>deliver(packet),local);
  server.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  server.listen(0,'127.0.0.1');await once(server,'listening');
  const client=net.connect(server.address().port,'127.0.0.1');await once(client,'connect');
  t.after(async()=>{client.destroy();for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));});
  return {client,admitted};
}
test('HTTP pins Host and forwards a bounded request with connection close',async t=>{
  const {client,admitted}=await fixture(t);
  client.write('POST http://127.0.0.1:4321/form HTTP/1.1\r\nHost: 127.0.0.1:4321\r\nContent-Length: 3\r\nProxy-Authorization: secret\r\nConnection: keep-alive\r\n\r\nabc');
  const packet=await admitted;assert.equal(packet.transport,'http');assert.equal(packet.connect,false);
  assert.match(packet.head.toString(),/^POST \/form HTTP\/1.1/);assert.match(packet.head.toString(),/Connection: close/);
  assert(!packet.head.includes('secret'));assert(packet.head.toString().endsWith('\r\n\r\nabc'));
});
test('Host mismatch is refused before any egress admission',async t=>{
  const {client,admitted}=await fixture(t);let dispatched=false;void admitted.then(()=>dispatched=true);
  client.write('GET http://127.0.0.1:4321/ HTTP/1.1\r\nHost: localhost:4321\r\n\r\n');
  await once(client,'close');assert.equal(dispatched,false);
});
test('HTTP rejects bare line breaks, control bytes and invalid header names before admission',async t=>{
  for(const line of ['X: a\nInjected: b','X: a\rInjected: b','X: a\0b','Bad Name: value','X: a\x7fb']){
    const {client,admitted}=await fixture(t);let dispatched=false;void admitted.then(()=>dispatched=true);
    client.write(`GET http://127.0.0.1:4321/ HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n${line}\r\n\r\n`);
    await Promise.race([once(client,'close'),admitted.then(()=>{throw new Error('malformed HTTP admitted');})]);assert.equal(dispatched,false,line);
  }
});
test('local WS termination rejects malformed headers and request targets before admission',async t=>{
  const cases=['X: a\nInjected: b','X: a\rInjected: b','X: a\0b','Bad Name: value'].map(line=>['GET /socket HTTP/1.1',line]);
  cases.push(['GET /socket\nInjected:a HTTP/1.1','X: value'],['GET /socket\rInjected:a HTTP/1.1','X: value']);
  for(const [request,line]of cases){
    const {client,admitted}=await fixture(t,new URL('http://127.0.0.1:4321'));let dispatched=false;void admitted.then(()=>dispatched=true);
    const response=once(client,'data');client.write('CONNECT 127.0.0.1:4321 HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\n');await response;
    client.write(`${request}\r\nHost: 127.0.0.1:4321\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n${line}\r\n\r\n`);
    await Promise.race([once(client,'close'),admitted.then(()=>{throw new Error('malformed WS admitted');})]);assert.equal(dispatched,false,line);
  }
});
test('Chromium local WS CONNECT requires exact Host and HTTP upgrade',async t=>{
  const {client,admitted}=await fixture(t,new URL('http://127.0.0.1:4321'));
  const response=once(client,'data');client.write('CONNECT 127.0.0.1:4321 HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\n');
  assert.match((await response)[0].toString(),/200 Connection Established/);
  client.write('GET /socket HTTP/1.1\r\nHost: 127.0.0.1:4321\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n');
  const packet=await admitted;assert.equal(packet.transport,'ws');assert.equal(packet.connect,false);assert.match(packet.head.toString(),/GET \/socket/);
});
test('local CONNECT cannot forward a TLS or non-upgrade request',async t=>{
  const {client,admitted}=await fixture(t,new URL('http://127.0.0.1:4321'));let dispatched=false;void admitted.then(()=>dispatched=true);
  const response=once(client,'data');client.write('CONNECT 127.0.0.1:4321 HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\n');await response;
  client.write('GET / HTTP/1.1\r\nHost: 127.0.0.1:4321\r\n\r\n');await once(client,'close');assert.equal(dispatched,false);
});

test('public CONNECT preserves explicit default 443 authority',async t=>{
  const {client,admitted}=await fixture(t);
  client.write('CONNECT public.example:443 HTTP/1.1\r\nHost: public.example:443\r\n\r\n');
  const packet=await admitted;assert.equal(packet.transport,'connect');assert.equal(packet.port,443);assert.equal(packet.host,'public.example');assert.equal(packet.connect,true);
});
