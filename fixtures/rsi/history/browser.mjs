import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService} from '../web-product/service.mjs';
const require=createRequire(new URL('../web-product/package.json',import.meta.url));const {chromium,firefox}=require('playwright');
const binary=resolve(process.env.RSI_BINARY??'target/debug/rsi'),assets=process.env.RSI_WEB_ASSETS;assert(assets);const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});const results=[];
for(const [name,engine]of[['chromium',chromium],['firefox',firefox]]){
 const report=join(output,name);await mkdir(report,{recursive:true});const service=await startService({binary,assets,report});const browser=await engine.launch({headless:true});let page;const errors=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30_000);page.on('pageerror',e=>errors.push(e.message));
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} history`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});const input=pane.getByRole('textbox',{name:'Main message',exact:true});const text='archived anchor 中文🦀材料';await input.fill(text);await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await pane.locator('.transcript').getByText(`Reviewed: ${text}`,{exact:false}).waitFor();
  const source=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
  await page.locator('#workspaces .nav-item').first().click();await input.fill('preserved draft');
  const target=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);assert.notEqual(source,target);
  await pane.getByRole('button',{name:'Search history',exact:true}).click();const dialog=page.getByRole('dialog',{name:'Search conversation text',exact:true});await dialog.getByLabel('History conversation ID',{exact:true}).fill(source);await dialog.getByLabel('History text query',{exact:true}).fill('anchor');
  await dialog.getByRole('button',{name:'Search text',exact:true}).click();await dialog.getByText('No matches in the indexed portion.',{exact:true}).waitFor();assert.match(await dialog.getByRole('status').innerText(),/More indexing needed/);
  await dialog.getByRole('button',{name:'Index next batch',exact:true}).click();await dialog.getByRole('status').filter({hasText:'Caught up at this observation'}).waitFor();await dialog.getByRole('button',{name:'Search text',exact:true}).click();await dialog.getByRole('button',{name:'Open original',exact:true}).first().waitFor();await page.screenshot({path:join(report,'matches.png')});
  await dialog.getByRole('button',{name:'Open original',exact:true}).first().click();const original=dialog.getByLabel('Verified original text',{exact:true});await original.waitFor();assert.equal(await original.inputValue(),text);
  await original.evaluate(element=>{const from=element.value.indexOf('中文🦀材料');element.focus();element.setSelectionRange(from,from+'中文🦀材料'.length);});await page.screenshot({path:join(report,'original-selection.png')});
  await dialog.getByRole('button',{name:'Freeze selected fragment',exact:true}).click();await dialog.getByRole('button',{name:'Add selected reference to draft',exact:true}).waitFor();assert.equal(await dialog.locator('pre').innerText(),'中文🦀材料');await page.setViewportSize({width:430,height:900});await page.screenshot({path:join(report,'frozen-narrow.png')});assert(await dialog.evaluate(e=>e.scrollWidth<=e.clientWidth+1));
  await dialog.getByRole('button',{name:'Add selected reference to draft',exact:true}).click();assert.equal(await input.inputValue(),'preserved draft');await pane.getByRole('button',{name:'Preview reference',exact:true}).click();const frozen=page.getByRole('dialog',{name:'Frozen conversation reference',exact:true});await frozen.locator('pre').filter({hasText:'中文🦀材料'}).waitFor();await page.screenshot({path:join(report,'draft-reference.png')});assert.equal(service.provider.requests.length,1,'search/read/freeze must not invoke a model');assert.deepEqual(errors,[]);
  results.push({browser:name,version:browser.version(),status:'passed',source,target,selected:'中文🦀材料',provider_requests:service.provider.requests.length});
 }catch(error){if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});throw error;}finally{await browser.close();await service.close();}
}
await writeFile(join(output,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
