import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService,boundedRun,waitUntil} from '../web-product/service.mjs';
const require=createRequire(new URL('../web-product/package.json',import.meta.url));const {chromium,firefox}=require('playwright');
const analyzer=process.env.RSI_RUST_ANALYZER;assert(analyzer);const binary=resolve(process.env.RSI_BINARY??'target/debug/rsi'),assets=process.env.RSI_WEB_ASSETS;assert(assets);const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});const results=[];
for(const [name,engine]of (process.env.RSI_LSP_BROWSER==='chromium'?[['chromium',chromium]]:[['chromium',chromium],['firefox',firefox]])){
 const report=join(output,name);await mkdir(report,{recursive:true});let position;const service=await startService({binary,assets,report,configure:async({config,workspace})=>{
  const result=boundedRun('python3',[resolve('fixtures/rsi/lsp/prepare.py'),workspace,join(config,'host-profiles/fixture/host.profile.toml'),analyzer]);position=JSON.parse(result.stdout);
 }});const browser=await engine.launch({headless:true});let page;const errors=[],calls=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();await page.addInitScript(()=>{window.languageTrace=[];const trace=value=>{if(window.languageTrace.length===128)window.languageTrace.shift();window.languageTrace.push(value);};const post=Worker.prototype.postMessage;Worker.prototype.postMessage=function(data,...args){if(!this.languageObserved){this.languageObserved=true;this.addEventListener('message',e=>{const text=JSON.stringify(e.data);if(text.includes('ui_detail'))trace({received:text.slice(text.indexOf('ui_detail'),text.indexOf('ui_detail')+6000)});});}if(JSON.stringify(data).includes('ui_invoke'))trace(data);return post.call(this,data,...args);};document.addEventListener('click',event=>{if(event.target.tagName==='BUTTON'&&event.target.closest('dialog'))trace({clicked:event.target.textContent,fields:[...document.querySelectorAll('dialog input')].map(e=>[e.getAttribute('aria-label'),e.value])});});});page.setDefaultTimeout(45_000);page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>{const body=r.postData();if(body?.includes('ui_invoke'))calls.push({url:r.url(),body});});
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} language`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
  await page.getByRole('button',{name:'Service extensions',exact:true}).click();const detail=page.getByRole('dialog');await detail.getByRole('button',{name:'Code intelligence',exact:true}).click();
  await detail.getByRole('textbox',{name:'Line',exact:true}).fill(String(position.line));await detail.getByRole('textbox',{name:'Column',exact:true}).fill(String(position.column));await detail.getByRole('button',{name:'Find definition',exact:true}).click();
  await waitUntil(async()=>{if(await detail.getByRole('button',{name:'Open src/main.rs:2 (UTF-16 column 12)',exact:true}).count())return true;const retry=detail.getByRole('button',{name:'Repeat query',exact:true});if(await retry.count()&&await retry.isEnabled())await retry.click();return false;},'language server returns actual definition',45_000);
  await detail.getByRole('button',{name:'Open src/main.rs:2 (UTF-16 column 12)',exact:true}).click();
  await waitUntil(async()=>(await detail.innerText()).includes(position.definition),'opened definition');assert.match(await detail.locator('pre').innerText(),/^pub struct Bird;/);await page.screenshot({path:join(report,'definition.png')});
  await page.setViewportSize({width:430,height:900});await page.screenshot({path:join(report,'definition-narrow.png')});assert(await detail.evaluate(e=>e.scrollWidth<=e.clientWidth+1));
  await detail.getByRole('button',{name:'New language query',exact:true}).click();await detail.getByRole('textbox',{name:'Line',exact:true}).fill(String(position.line));await detail.getByRole('textbox',{name:'Column',exact:true}).fill(String(position.column));await detail.getByRole('button',{name:'Read hover',exact:true}).click();await waitUntil(async()=>await detail.locator('pre').count()&&(await detail.locator('pre').innerText()).includes('Bird'),'real hover');await page.screenshot({path:join(report,'hover-narrow.png')});
  await detail.getByRole('button',{name:'New language query',exact:true}).click();await detail.getByRole('textbox',{name:'Line',exact:true}).fill(String(position.line));await detail.getByRole('textbox',{name:'Column',exact:true}).fill(String(position.column));await detail.getByRole('button',{name:'Find references',exact:true}).click();
  await waitUntil(async()=>{if(await detail.getByRole('button',{name:'More locations',exact:true}).count())return true;const retry=detail.getByRole('button',{name:'Repeat query',exact:true});if(await retry.count()&&await retry.isEnabled())await retry.click();return false;},'reference result spans multiple pages',45_000);
  const original=await readFile(join(service.workspace,'src/main.rs'),'utf8');const first=await detail.getByRole('button',{name:/^Open /}).allTextContents();assert.equal(first.length,16);
  try {
   // A new query must now fail source admission. Paging must still use the
   // previous result, proving that the UI does not silently query again.
   await writeFile(join(service.workspace,'src/main.rs'),'x'.repeat(1024*1024+1));
   await detail.getByRole('button',{name:'More locations',exact:true}).click();
   await waitUntil(async()=>{const labels=await detail.getByRole('button',{name:/^Open /}).allTextContents();return labels.length===16&&labels.every(label=>!first.includes(label));},'cached second page survives source replacement');
   await page.screenshot({path:join(report,'references-cached-page.png')});
   await detail.getByRole('button',{name:'Repeat query',exact:true}).click();
   await waitUntil(async()=>(await detail.innerText()).includes('Language read failed:'),'explicit repeat checks current source');
   await page.screenshot({path:join(report,'references-repeat-error.png')});
  } finally {await writeFile(join(service.workspace,'src/main.rs'),original);}
  await detail.getByRole('button',{name:'Repeat query',exact:true}).click();
  await waitUntil(async()=>{if((await detail.getByRole('button',{name:/^Open /}).count())===16)return true;const retry=detail.getByRole('button',{name:'Repeat query',exact:true});if(await retry.count()&&await retry.isEnabled())await retry.click();return false;},'explicit repeat recovers after source restore');
  assert.equal(service.provider.requests.length,0);assert.deepEqual(errors,[]);results.push({browser:name,version:browser.version(),server:position.version,status:'passed',location:position.definition,provider_requests:0,cached_page_survives_source_replacement:true,explicit_repeat_revalidates_source:true,source:await readFile(join(service.workspace,'src/main.rs'),'utf8')});
 }catch(error){if(page){await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),await page.locator('body').innerText());await writeFile(join(report,'calls.json'),JSON.stringify({calls,trace:await page.evaluate(()=>window.languageTrace)},null,2));}throw error;}finally{await browser.close();await service.close();}
}
await writeFile(join(output,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
