import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,writeFile,mkdir,readdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
const [documents,report]=process.argv.slice(2).map(path=>resolve(path));
if(!documents||!report) throw new Error('Pass instrumented documents and report directory');
await mkdir(report,{recursive:false});
const results=[];
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]) for(const variant of ['baseline','current']) {
  const directory=join(documents,variant), files=new Map();
  for(const entry of await readdir(directory,{withFileTypes:true})) if(entry.isFile()) files.set('/'+entry.name,await readFile(join(directory,entry.name)));
  const server=createServer((request,response)=>{const path=new URL(request.url,'http://localhost').pathname,bytes=files.get(path==='/'?'/index.html':path);if(!bytes){response.writeHead(404).end();return}response.setHeader('Content-Type',path==='/'?'text/html':path.endsWith('.css')?'text/css':path.endsWith('.js')?'text/javascript':'application/octet-stream');response.end(bytes)});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:980}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}`);await page.waitForFunction(()=>!!window.rsiPerformance);
    for(const count of [16,64,128]) {
      for(let run=0;run<10;run++) {
        const setup=await page.evaluate(([count,run])=>window.rsiPerformance.setup(count,run),[count,run]);assert.equal(setup.blocks,count);assert(setup.input>=180);assert.match(setup.body,/Result 0:/);
        for(let key=0;key<5;key++) {const before=await page.evaluate(()=>window.rsiPerformance.samples.length);await page.keyboard.insertText('x');await page.waitForFunction(before=>window.rsiPerformance.samples.length>before,before)}
        assert.equal(await page.locator('textarea[aria-label$=message]').first().inputValue(),'xxxxx');
        assert.equal(await page.locator('.message-text').last().innerText(),'Streaming update xxxxx');
      }
      await page.screenshot({path:join(report,`${name}-${variant}-${count}.png`),fullPage:true});
    }
    const trajectory=await page.evaluate(()=>window.rsiPerformance.trajectory());assert.equal(trajectory.laidOut,128);assert(trajectory.tools>=40&&trajectory.reasoning>=40);
    await page.screenshot({path:join(report,`${name}-${variant}-trajectory-128.png`),fullPage:true});
    assert.deepEqual(errors,[]);
    const samples=await page.evaluate(()=>window.rsiPerformance.samples);
    const row={browser:name,version:browser.version(),variant,trajectory,samples,summary:[16,64,128].map(blocks=>{const values=samples.filter(sample=>sample.blocks===blocks).map(sample=>sample.input_to_paint_ms).sort((a,b)=>a-b);return {blocks,samples:values.length,p95_ms:values[Math.ceil(values.length*.95)-1]}})};
    results.push(row);await writeFile(join(report,`${name}-${variant}.json`),JSON.stringify(row,null,2));
  } catch(error) {await page.screenshot({path:join(report,`${name}-${variant}-failure.png`)}).catch(()=>{});throw error}
  finally {await browser.close();await new Promise(resolve=>server.close(resolve))}
}
await writeFile(join(report,'summary.json'),JSON.stringify(results.map(({samples,...row})=>row),null,2));
