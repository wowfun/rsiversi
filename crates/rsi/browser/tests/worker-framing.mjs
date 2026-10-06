import {test} from 'node:test';
import assert from 'node:assert/strict';
import {NullFrameReader} from '../runtime/frame-reader.mjs';
test('fragmented maximum frame, multiple delimiters and owned response bytes',()=>{
 const maximum=8*1024*1024,reader=new NullFrameReader(maximum),frames=[];
 const receive=frame=>frames.push(frame);
 const fragment=Buffer.alloc(65536,97);
 for(let i=0;i<128;i++)reader.push(fragment,receive);
 assert.equal(frames.length,0);
 reader.push(Buffer.from([0,98,0,99]),receive);
 assert.equal(frames[0].length,maximum);assert.equal(frames[0][0],97);assert.equal(frames[0][maximum-1],97);
 assert.equal(frames[1].toString(),'b');
 reader.push(Buffer.from([100,0]),receive);assert.equal(frames[2].toString(),'cd');
 assert.equal(frames[0][0],97,'later frames cannot overwrite published response bytes');
 const tooLarge=new NullFrameReader(3);tooLarge.push(Buffer.from('abc'),()=>{});
 assert.throws(()=>tooLarge.push(Buffer.from('d'),()=>{}),/bound/);
});
