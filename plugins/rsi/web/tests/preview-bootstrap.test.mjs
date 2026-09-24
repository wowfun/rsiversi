import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import * as bounds from '../image-bounds.js';
import { png } from '../../../../fixtures/rsi/web-product/images.mjs';
const source=(await readFile(new URL('../preview-bootstrap.js',import.meta.url),'utf8')).replace(/^import[^\n]+\n/,'');

test('HTML rejects aggregate image dimensions before writing any user document',()=>{
  let listener;const parent={postMessage(){}};const writes=[],responses=[];
  const context={...bounds,parent,location:{protocol:'https:',host:'localhost',pathname:'/preview-local.html'},window:{addEventListener(_kind,callback){listener=callback;}},document:{body:{},open(){},write(text){writes.push(text);},close(){}},TextDecoder,Uint8Array,Map,Set,Blob,URL:{createObjectURL(){return 'blob:bounded';}}};
  vm.runInNewContext(source,context);
  const port={start(){},close(){},postMessage(message){responses.push(message);}};
  listener({source:parent,origin:'https://localhost',data:{type:'rsi-preview-connect'},ports:[port]});
  const data=png(1,1,[1,2,3,255]);data.writeUInt32BE(4096,16);data.writeUInt32BE(4096,20);
  port.onmessage({data:{type:'render',html:[0,1,2].map(i=>`<img src="rsi-preview-resource:${i}">`).join(''),assets:[0,1,2].map(i=>({name:`asset-${i}`,mime:'image/png',data}))}});
  assert.equal(writes.length,0);
  assert.match(responses[0].message,/aggregate pixel/);
});

test('missing local resources preserve the document and remaining resources',()=>{
  let listener;const parent={postMessage(){}};const writes=[],responses=[];
  const context={...bounds,parent,location:{protocol:'https:',host:'localhost',pathname:'/preview-local.html'},window:{addEventListener(_kind,callback){listener=callback;}},document:{body:{},open(){},write(text){writes.push(text);},close(){}},TextDecoder,Uint8Array,Map,Set,Blob,URL:{createObjectURL(){return 'blob:approved';}}};
  vm.runInNewContext(source,context);
  const port={start(){},close(){},postMessage(message){responses.push(message);}};
  listener({source:parent,origin:'https://localhost',data:{type:'rsi-preview-connect'},ports:[port]});
  port.onmessage({data:{type:'render',html:'<h1>Still visible</h1><img alt="Missing" src="rsi-preview-resource:0"><link href="rsi-preview-resource:1">',assets:[{name:'asset-1',mime:'text/css',data:new TextEncoder().encode('body{background:url(rsi-preview-resource:2)}')}]}});
  assert.deepEqual(writes,['<h1>Still visible</h1><img alt="Missing" src="about:blank"><link href="blob:approved">']);
  assert.equal(responses[0].type,'ready');
});

test('opaque bootstrap rejects foreign, null and sibling Origins before consuming its one-use port',()=>{
  for(const protocol of ['https:','rsi:']){
    let listener;const parent={postMessage(){}};const writes=[];
    const context={...bounds,parent,location:{protocol,host:'localhost',pathname:'/preview-local.html'},window:{addEventListener(_kind,callback){listener=callback;}},document:{body:{},open(){assert.equal(closed,1,'port must close before document.open');},write(text){writes.push(text);},close(){}},TextDecoder,Uint8Array,Map,Set};
    vm.runInNewContext(source,context);
    const rejected=[];
    for(const origin of ['null',`${protocol}//foreign`,`${protocol}//localhost.evil`,`${protocol}//localhost:99`]){
      const port={start(){rejected.push(origin);},close(){}};
      listener({source:parent,origin,data:{type:'rsi-preview-connect'},ports:[port]});
      assert.equal(port.onmessage,undefined);
    }
    assert.deepEqual(rejected,[]);
    let started=0,closed=0;const port={start(){started++;},close(){closed++;},postMessage(){}};
    listener({source:parent,origin:`${protocol}//localhost`,data:{type:'rsi-preview-connect'},ports:[port]});
    assert.equal(started,1);
    port.onmessage({data:{type:'render',html:'<h1>Trusted parent data</h1>',assets:[]}});
    assert.equal(closed,1);assert.deepEqual(writes,['<h1>Trusted parent data</h1>']);
    const second={start(){throw new Error('Second port admitted');}};
    listener({source:parent,origin:`${protocol}//localhost`,data:{type:'rsi-preview-connect'},ports:[second]});
  }
});

test('HTML resources cannot disguise raster payloads with a non-image MIME',()=>{
  for(const mime of ['image/png','text/plain','font/woff2']) {
    let listener; const parent={postMessage(){}}; const writes=[],responses=[],blobs=[];
    const context={...bounds,parent,location:{protocol:'https:',host:'localhost',pathname:'/preview-local.html'},window:{addEventListener(_kind,callback){listener=callback;}},document:{body:{},open(){},write(text){writes.push(text);},close(){}},TextDecoder,Uint8Array,Map,Set,Blob,URL:{createObjectURL(blob){blobs.push(blob);return 'blob:unsafe';}}};
    vm.runInNewContext(source,context);
    let closed=0;const port={start(){},close(){closed++;},postMessage(message){responses.push(message);}};
    listener({source:parent,origin:'https://localhost',data:{type:'rsi-preview-connect'},ports:[port]});
    const oversized=png(1,1,[0,0,0,255]);oversized.writeUInt32BE(100000,16);oversized.writeUInt32BE(100000,20);
    port.onmessage({data:{type:'render',html:'<img src="rsi-preview-resource:0">',assets:[{name:'asset-0',mime,data:oversized}]}});
    assert.equal(closed,1);assert.equal(writes.length,0,mime);assert.equal(blobs.length,0,mime);
    assert.match(responses[0].message,/pixel limit/);assert.equal(context.document.body.textContent,'HTML preview unavailable');
  }
});
