import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer, request} from 'node:http';
import {connect} from 'node:net';
import {createHash} from 'node:crypto';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
import {decodeFrame} from '../development-control.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const run = '0123456789abcdef0123456789abcdef';
const family = 'a'.repeat(64);
function frame(message) { return JSON.stringify({version: 1, run_id: run, message}) + '\n'; }
function child() {
  const environment = Object.fromEntries(['PATH','Path','SystemRoot','WINDIR','TEMP','TMP','HOME','USERPROFILE','LOCALAPPDATA','APPDATA'].filter(key => process.env[key]).map(key => [key,process.env[key]]));
  const child = spawn(process.execPath, ['development-server.mjs'], {cwd: root, env: environment, stdio: ['pipe','pipe','pipe']});
  const lines = createInterface({input: child.stdout}); let diagnostics = '';
  child.stderr.on('data', bytes => { diagnostics = (diagnostics + bytes).slice(-16384); });
  const iterator = lines[Symbol.asyncIterator]();
  const exited = once(child, 'exit');
  return {
    child, exited,
    send(message) { child.stdin.write(frame(message)); },
    async next() { const line = await iterator.next(); assert(!line.done, diagnostics); return JSON.parse(line.value).message; },
    async close() { child.stdin.end(); await Promise.race([exited, new Promise((_, reject) => { const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error('development child did not settle: ' + diagnostics)); }, 25000); timer.unref(); })]); },
  };
}
function get(origin, path, headers = {}) {
  return new Promise((resolve, reject) => {
    const call = request(origin + path, {headers}, response => { let body='';response.on('data',bytes=>{body+=bytes});response.on('end',()=>resolve({status:response.statusCode,body})); });
    call.on('error',reject);call.end();
  });
}
test('control rejects unknown, duplicate, non-loopback and oversized frames', () => {
  const good = frame({event:'start_frontend',port:0,family});
  assert.equal(decodeFrame(Buffer.from(good)).message.port,0);
  for (const invalid of [
    {version:1,run_id:[run],message:{event:'start_frontend',port:0,family}},
    {version:1,run_id:run,message:{event:['start_frontend'],port:0,family}},
    {version:1,run_id:run,message:{event:'start_frontend',port:0,family:[family]}},
    {version:1,run_id:run,message:{event:'frontend_bound',origin:'https://foreign.invalid'}},
  ]) assert.throws(()=>decodeFrame(Buffer.from(JSON.stringify(invalid)+'\n')));
  for (const source of [good.replace('"port":0','"port":0,"port":1'), frame({event:'configure_proxy',upstream:'http://foreign.invalid'}), frame({event:'stop',extra:1}), 'x'.repeat(32769)+'\n', good.replace('"version":1','"version":1,"version":1')]) assert.throws(()=>decodeFrame(Buffer.from(source)));
});
test('owned Vite socket gates requests then preserves HTTP and WebSocket authority', {timeout:60000}, async () => {
  const seen=[];
  const upstream=createServer((request,response)=>{seen.push(request.headers);response.writeHead(200,{'Content-Type':'application/json'}).end(JSON.stringify(request.headers));});
  const sockets=new Set();upstream.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
  upstream.on('upgrade',(request,socket)=>{
    seen.push(request.headers);
    const accept=createHash('sha1').update(request.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.end('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+accept+'\r\nSec-WebSocket-Protocol: '+request.headers['sec-websocket-protocol']+'\r\n\r\n');
  });
  upstream.listen(0,'127.0.0.1');await once(upstream,'listening');
  const peer=child();
  try {
    peer.send({event:'start_frontend',port:0,family});const bound=await peer.next();assert.equal(bound.event,'frontend_bound');
    const origin=bound.origin;
    assert.equal((await get(origin,'/')).status,503);assert.equal(seen.length,0);
    peer.send({event:'configure_proxy',upstream:`http://127.0.0.1:${upstream.address().port}`});assert.equal((await peer.next()).event,'proxy_ready');
    const response=await get(origin,'/api/v1/probe',{Origin:origin,'X-Rsi-Csrf':'1'});assert.equal(response.status,200);
    assert.equal(JSON.parse(response.body).host,new URL(origin).host);assert.equal(JSON.parse(response.body).origin,origin);
    const foreign=await get(origin,'/api/v1/probe',{Origin:'http://foreign.invalid'});assert.equal(JSON.parse(foreign.body).origin,'http://foreign.invalid');
    const address=new URL(origin);const socket=connect(Number(address.port),'127.0.0.1');await once(socket,'connect');
    socket.write(`GET /api/v1/stream HTTP/1.1\r\nHost: ${address.host}\r\nOrigin: ${origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ZGV2ZWxvcG1lbnQtdGVzdA==\r\nSec-WebSocket-Protocol: rsi-stream-v1\r\n\r\n`);
    const [bytes]=await once(socket,'data');assert.match(bytes.toString(),/101 Switching Protocols/);assert.match(bytes.toString(),/Sec-WebSocket-Protocol: rsi-stream-v1/i);socket.destroy();
    assert.equal(seen.at(-1).origin,origin);assert.equal(seen.at(-1)['sec-websocket-protocol'],'rsi-stream-v1');
    assert.equal((await get(origin,'/')).status,200);
    assert.match((await get(origin,'/@vite/client')).body,/WebSocket/);
  } finally {try {await peer.close();} finally {for(const socket of sockets)socket.destroy();await new Promise(resolve=>upstream.close(resolve));}}
});
test('occupied frontend port cannot publish readiness', {timeout:15000}, async () => {
  const owner=createServer((_request,response)=>response.end('occupied'));owner.listen(0,'127.0.0.1');await once(owner,'listening');
  const peer=child();const output=[];peer.child.stdout.on('data',bytes=>output.push(bytes));
  try {peer.send({event:'start_frontend',port:owner.address().port,family});const [code]=await peer.exited;assert.notEqual(code,0);assert.equal(Buffer.concat(output).length,0);}
  finally {await peer.close();await new Promise(resolve=>owner.close(resolve));}
});

test('two port-zero frontends bind distinct owned sockets', {timeout:60000}, async () => {
  const first=child(),second=child();
  try {
    first.send({event:'start_frontend',port:0,family});
    const a=await first.next();
    second.send({event:'start_frontend',port:0,family});
    const b=await second.next();
    assert.equal(a.event,'frontend_bound');assert.equal(b.event,'frontend_bound');
    assert.notEqual(a.origin,b.origin);
    assert.equal((await get(a.origin,'/')).status,503);
    assert.equal((await get(b.origin,'/')).status,503);
  } finally {await Promise.all([first.close(),second.close()]);}
});
