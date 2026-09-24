import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {createReadStream} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';

// Shipped Service Worker and document adapter, with a gated event producer.
// Rust framing and Store behavior are tested separately at their public seams.
export async function verifyDownloadStream(browser,root,trace=()=>{}) {
  const files=Object.fromEntries(await Promise.all(['download-worker.js','download-frame.js','session-export.js'].map(async name=>['/'+name,await readFile(join(root,'plugins/rsi/web',name))])));
  const server=createServer((request,response)=>{response.setHeader('Content-Type',request.url.endsWith('.js')?'text/javascript':'text/html');response.end(files[request.url]??'<!doctype html><body>Streaming download fixture</body>');});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const context=await browser.newContext(),page=await context.newPage();page.setDefaultTimeout(15000);
  const chunk='界🦀'.repeat(8192), count=600;
  const browserFailures={};
  const expected=createHash('sha256');for(let n=0;n<count;n++)expected.update(chunk);
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(()=>{
      const NativeChannel=window.MessageChannel;
      window.MessageChannel=class extends NativeChannel {constructor(){super();this.port1.addEventListener('message',event=>{if(event.data.kind==='aborted')window.downloadProbe.aborted=true;});}};
    });
    for(const mode of ['large','cancel','eof']) {
      trace({mode,stage:'start'});
      const incoming=page.waitForEvent('download');
      await page.evaluate(async({mode,chunk,count})=>{
        URL.createObjectURL=()=>{throw new Error('Export must never create a Blob URL');};
        const {downloadSession}=await import('/session-export.js');
        let reads=0,active=0,maximum=0,release;
        const stop=new AbortController();window.cancelExport=()=>stop.abort();window.downloadProbe={}; window.downloadSettled=undefined;
        window.downloadResult=downloadSession(async(method,input)=>{
          const op=JSON.parse(input).operation;
          if(op.kind==='open')return JSON.stringify({token:mode,filename:`${mode}.md`});
          if(op.kind==='cancel'){release?.();window.downloadProbe.cancelled=true;return 'null';}
          active++;maximum=Math.max(maximum,active);reads++;window.downloadProbe.reads=reads;
          let item;
          if(reads===1||mode==='large'&&reads<=count)item={type:'chunk',text:chunk};
          else if(mode==='cancel'){window.downloadProbe.pending=true;await new Promise(resolve=>{release=resolve;});item=null;}
          else if(mode==='large'&&reads===count+1)item={type:'complete'};
          else item=null;
          active--;window.downloadProbe.maximum=maximum;
          return JSON.stringify(item);
        },'main','1','',stop.signal).then(()=>({status:'passed'}),error=>({status:'failed',error:String(error)})).then(result=>{window.downloadSettled=result;return result;});
      },{mode,chunk,count});
      const download=await incoming;
      trace({mode,stage:'received'});
      let deadline;
      const failure=()=>Promise.race([download.failure(),new Promise(resolve=>{deadline=setTimeout(()=>resolve('pending'),10000);})]).finally(()=>clearTimeout(deadline));
      if(mode==='large') {
        assert.equal(await failure(),null);
        const hash=createHash('sha256');let bytes=0;
        for await(const part of createReadStream(await download.path())){bytes+=part.length;hash.update(part);}
        assert.equal(bytes,Buffer.byteLength(chunk)*count);assert(bytes>32*1024*1024);
        assert.equal(hash.digest('hex'),expected.digest('hex'));
        assert.equal((await page.evaluate(()=>window.downloadResult)).status,'passed');
        await page.evaluate(url=>{const replay=document.createElement('iframe');replay.id='replay';replay.src=url;document.body.append(replay);},download.url());
        assert.equal(await page.frameLocator('#replay').locator('body').innerText(),'Download unavailable');
        await page.locator('#replay').evaluate(frame=>frame.remove());
      } else {
        if(mode==='cancel'){await page.waitForFunction(()=>window.downloadProbe.pending);assert.equal(await page.evaluate(()=>window.downloadProbe.reads),2);await page.evaluate(()=>window.cancelExport());}
        assert.equal((await page.evaluate(()=>window.downloadResult)).status,'failed');
        // The application and SW must fail immediately and release the producer.
        // Also record the download manager outcome: Firefox can leave its failed
        // download pending even after the SW has acknowledged response.error().
        assert.equal(await page.evaluate(()=>window.downloadProbe.aborted),true);
        browserFailures[mode]=await failure();
        assert.notEqual(browserFailures[mode],null,'A truncated artifact cannot complete successfully');
        if(browserFailures[mode]==='pending')await download.cancel();
      }
      const probe=await page.evaluate(()=>window.downloadProbe);
      assert.equal(probe.maximum,1);assert(probe.cancelled);
    }
    assert.equal(await page.evaluate(()=>navigator.serviceWorker.controller),null);
    assert.deepEqual(await page.evaluate(()=>caches.keys()),[]);
    return {largeBytes:Buffer.byteLength(chunk)*count,maximumConcurrentReads:1,cancel:true,abnormalEof:true,noBlob:true,oneUse:true,browserFailures};
  } finally {await context.close();await new Promise(resolve=>server.close(resolve));}
}
