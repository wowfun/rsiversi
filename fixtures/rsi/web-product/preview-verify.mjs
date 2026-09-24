import assert from 'node:assert/strict';
import {mkdir,writeFile,copyFile,chmod,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
import {verifyFilePreviews} from './file-previews.mjs';
import {verifyMarkdownAgents} from './markdown-agents.mjs';
const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS,sourceBinary=process.env.RSI_WEB_BINARY;
assert(report&&assets&&sourceBinary,'explicit isolated inputs required');await mkdir(report,{recursive:true});
const binary=join(report,'rsi');await copyFile(sourceBinary,binary);await chmod(binary,0o700);await writeFile(join(report,'binary.json'),JSON.stringify({source:sourceBinary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex')}));
for(const[name,engine]of[['chromium',chromium],['firefox',firefox]]){
 if(process.env.RSI_WEB_BROWSER&&process.env.RSI_WEB_BROWSER!==name)continue;
 const directory=join(report,name);await mkdir(directory,{recursive:true});
 const providerBodies=[];const service=await startService({binary,assets,report:directory,onRequest:body=>providerBodies.push(body)}),browser=await engine.launch();let page;const errors=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});await context.addInitScript(()=>{
   const create=URL.createObjectURL.bind(URL),revoke=URL.revokeObjectURL.bind(URL);window.previewUrls=new Map();
   URL.createObjectURL=blob=>{const url=create(blob);window.previewUrls.set(url,blob.size);return url;};
   URL.revokeObjectURL=url=>{window.previewUrls.delete(url);revoke(url);};
  });page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} preview verification`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
  const agents=await verifyMarkdownAgents(page,service,directory,name,providerBodies);
  const evidence=await verifyFilePreviews(page,service,directory,name);
  await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});assert.deepEqual(errors,[]);await writeFile(join(directory,'result.json'),JSON.stringify({status:'passed',browser:browser.version(),agents,...evidence},null,2));
 }catch(error){await page?.screenshot({path:join(directory,'failure.png')});await writeFile(join(directory,'failure.html'),await page?.content()??'');await writeFile(join(directory,'failure.txt'),String(error));throw error;}
 finally{await browser.close();await service.close();}
}
