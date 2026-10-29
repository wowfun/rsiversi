import {test} from 'node:test';
import assert from 'node:assert/strict';
import {accessKey,readAccess,saveAccess} from '../composer-access.js';
test('access choices are isolated by connection and session, bounded, and reject malformed storage',()=>{
  let raw; const storage={getItem:()=>raw,setItem:(_,value)=>{raw=value;}};
  const a=accessKey(['host','principal'],'session'),b=accessKey(['other','principal'],'session');
  saveAccess(a,'read-only',storage);assert.equal(readAccess(a,storage),'read-only');assert.equal(readAccess(b,storage),undefined);
  assert.throws(()=>saveAccess(a,'auto',storage));assert.equal(readAccess(a,storage),'read-only');
  for(let i=0;i<128;i++)saveAccess(accessKey(['host','principal'],String(i)),'workspace-write',storage);
  assert.equal(readAccess(a,storage),undefined);assert.equal(JSON.parse(raw).length,128);
  raw='[["x","unrestricted"]]';assert.throws(()=>readAccess(a,storage));
});
