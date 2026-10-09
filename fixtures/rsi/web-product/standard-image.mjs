// Keyless renderer lifecycle evidence; no Service, provider or native confinement claim.
import assert from 'node:assert/strict';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {chromium} from 'playwright';
const root=resolve('.'),report=process.env.RSI_REPORT_DIR;
assert(report,'explicit new RSI_REPORT_DIR required');await mkdir(report,{recursive:false});
const browser=await chromium.launch();
try {
  const page=await browser.newPage();const errors=[];page.on('pageerror',error=>errors.push(error.message));
  const sources=new Map(await Promise.all(['mounts.js','standard.js'].map(async name=>[name,await readFile(join(root,'apps/web',name),'utf8')])));
  await page.route('http://renderer-image.test/**',route=>{const name=new URL(route.request().url()).pathname.split('/').at(-1);return route.fulfill({contentType:sources.has(name)?'text/javascript':'text/html',body:sources.get(name)??'<!doctype html><body>'});});
  await page.goto('http://renderer-image.test/');
  await page.evaluate(async()=>{
    const {MountTable}=await import('/mounts.js');
    const canvas=document.createElement('canvas');canvas.width=1280;canvas.height=720;const ctx=canvas.getContext('2d');ctx.fillStyle='#ffc83d';ctx.fillRect(0,0,1280,720);ctx.fillStyle='#222';ctx.font='48px sans-serif';ctx.fillText('Renderer lifecycle fixture',50,100);
    const png=new Uint8Array(await (await new Promise(resolve=>canvas.toBlob(resolve,'image/png'))).arrayBuffer());
    const root=document.createElement('section');document.body.append(root);
    const offer={revision:'a'.repeat(64),catalog:{renderers:[{id:'rsi.standard',abi:1,entry:'standard.js',files:[{name:'standard.js',sha256:'b'.repeat(64)}],schemas:[{name:'rsi.standard.view',version:1}],capabilities:['source'],surfaces:['pane']}]}};
    window.calls=[];window.releases={};window.rejects={};window.created=[];window.revoked=[];
    const create=URL.createObjectURL.bind(URL),revoke=URL.revokeObjectURL.bind(URL);
    URL.createObjectURL=blob=>{const url=create(blob);window.created.push(url);return url;};URL.revokeObjectURL=url=>{window.revoked.push(url);revoke(url);};
    const slot=name=>({key:'image',binding:'session',surface:'pane',root,snapshot:{revision:'1',busy:false,model:{renderer:'rsi.standard',schema:{name:'rsi.standard.view',version:1},actions:[],sources:[{name,media_type:'image/png'}],data:{image:{source:name,bytes:png.length,width:1280,height:720}},standard_view:{title:'Screenshot',elements:[]}}},host:{source:async(source,offset,maximum)=>{if(!root.isConnected)throw new Error('Read before DOM commit');if(maximum>65536)throw new Error('Unbounded source read');window.calls.push(source);if(source==='incomplete')return new Uint8Array();if(source==='invalid')return new Uint8Array(Math.min(maximum,png.length-offset));if(source==='rejected')throw new Error('Screenshot source refused');if(source!=='second')await new Promise((resolve,reject)=>{window.releases[source]=resolve;window.rejects[source]=reject;});return png.slice(offset,offset+maximum);}}});
    window.table=await MountTable.open();window.offer=offer;window.slot=slot;
    await window.table.render(offer,[slot('first')]);
  });
  await page.waitForFunction(()=>window.calls.includes('first'));
  await page.evaluate(()=>window.table.render(window.offer,[window.slot('second')]));
  await page.waitForFunction(()=>document.querySelector('img')?.naturalWidth===1280);
  const original=await page.locator('img').getAttribute('src');
  await page.evaluate(()=>{const next=window.slot('second');next.snapshot.revision='2';return window.table.render(window.offer,[next]);});
  await page.waitForFunction(()=>window.calls.filter(name=>name==='second').length===2&&document.querySelector('img')?.naturalWidth===1280);
  const current=await page.locator('img').getAttribute('src');assert.notEqual(current,original,'same model with a fresh revision did not reload its source');
  await page.evaluate(()=>window.releases.first());
  await page.waitForFunction(()=>window.table.entries.get('image').work.size===0);
  assert.equal(await page.locator('img').getAttribute('src'),current,'retired read replaced current image');
  const failures=[['incomplete','Incomplete screenshot source'],['invalid','Invalid screenshot source'],['rejected','Screenshot source refused']];
  for(const [source,message] of failures){
    await page.evaluate(source=>window.table.render(window.offer,[window.slot(source)]),source);
    await page.waitForFunction(message=>document.querySelector('.ui-screenshot .source-error')?.textContent===message,message);
    assert.equal(await page.locator('img').count(),0,'failed source retained an empty image');
  }
  await page.screenshot({path:join(report,'source-error.png')});
  await page.evaluate(()=>window.table.render(window.offer,[window.slot('retired-failure')]));
  await page.waitForFunction(()=>window.calls.includes('retired-failure'));
  await page.evaluate(()=>window.table.render(window.offer,[window.slot('second')]));
  await page.waitForFunction(()=>document.querySelector('img')?.naturalWidth===1280);
  const replacement=await page.locator('img').getAttribute('src');
  await page.evaluate(()=>window.rejects['retired-failure'](new Error('Retired screenshot failure')));
  await page.waitForFunction(()=>window.table.entries.get('image').work.size===0);
  assert.equal(await page.locator('.source-error').count(),0,'retired failure escaped into the replacement snapshot');
  assert.equal(await page.locator('img').getAttribute('src'),replacement);
  await page.screenshot({path:join(report,'replacement.png')});
  await page.evaluate(()=>window.table.render(window.offer,[window.slot('third')]));
  await page.waitForFunction(()=>window.calls.includes('third'));
  await page.evaluate(()=>{window.fixtureClosed=false;window.closing=window.table.close().then(()=>window.fixtureClosed=true);});
  assert.equal(await page.evaluate(()=>window.fixtureClosed),false,'Close lost the pending source work');
  await page.evaluate(()=>window.releases.third());await page.evaluate(()=>window.closing);
  assert.equal(await page.locator('img').count(),0);
  const result=await page.evaluate(()=>({calls:window.calls,created:window.created.length,revoked:window.revoked.length,closed:window.fixtureClosed}));
  assert.deepEqual(result,{calls:['first','second','second','incomplete','invalid','rejected','retired-failure','second','third'],created:3,revoked:3,closed:true});assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({...result,visibleFailures:failures.map(([,message])=>message),retiredFailureSuppressed:true,status:'passed',errors},null,2));
}finally{await browser.close();}
