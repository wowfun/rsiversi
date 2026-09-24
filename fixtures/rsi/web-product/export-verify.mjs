import assert from 'node:assert/strict';
import {mkdir,writeFile,copyFile,chmod,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
import {verifyExport} from './export.mjs';
import {verifyDownloadStream} from './download-stream.mjs';
import {resolve} from 'node:path';
const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS,sourceBinary=process.env.RSI_WEB_BINARY;
assert(report&&assets&&sourceBinary,'explicit isolated inputs required');await mkdir(report,{recursive:true});
const binary=join(report,'rsi');await copyFile(sourceBinary,binary);await chmod(binary,0o700);await writeFile(join(report,'binary.json'),JSON.stringify({source:sourceBinary,sha256:createHash('sha256').update(await readFile(binary)).digest('hex')}));
for(const[name,engine]of[['chromium',chromium],['firefox',firefox]]){
 if(process.env.RSI_WEB_BROWSER&&process.env.RSI_WEB_BROWSER!==name)continue;
 const directory=join(report,name);await mkdir(directory,{recursive:true});
 const service=await startService({binary,assets,report:directory}),browser=await engine.launch(name==='chromium'?{args:['--ignore-certificate-errors']}:{ });let page;const errors=[],requests=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));page.on('console',message=>{if(message.type()==='error')errors.push(message.text())});page.on('requestfailed',request=>requests.push(`${request.method()} ${new URL(request.url()).pathname}: ${request.failure()?.errorText}`));
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} export verification`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
  await verifyExport(page,service,directory,name);
  await writeFile(join(directory,'download-stream.json'),JSON.stringify(await verifyDownloadStream(browser,resolve(import.meta.dirname,'../../..'))));
  await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});
  // Downloads hand navigation to the manager and logout aborts live subscriptions.
  // Preserve network diagnostics; artifact completion and JS/CSP errors are the gates.
  await writeFile(join(directory,'network.json'),JSON.stringify(requests));assert.deepEqual(errors,[]);
 }catch(error){await page?.screenshot({path:join(directory,'failure.png')});await writeFile(join(directory,'failure.html'),await page?.content()??'');await writeFile(join(directory,'failure.txt'),String(error)+'\n'+errors.join('\n'));await writeFile(join(directory,'frames.json'),JSON.stringify(await Promise.all((page?.frames()??[]).map(async frame=>({url:frame.url(),content:await frame.content().catch(()=>null)})))));throw error;}
 finally{await browser.close();await service.close();}
}
