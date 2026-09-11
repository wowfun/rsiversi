// Run against an already-started, isolated cargo xtask dev web environment.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
const [directory,origin,report]=process.argv.slice(2);
if(!directory||!origin||!report)throw new Error('Pass isolated development directory, origin and new report directory');
await mkdir(report,{recursive:false});
const source=resolve('plugins/rsi/web/src/navigation.tsx'), bootstrap=resolve('plugins/rsi/web/app.js');
const original=await readFile(source,'utf8'), boot=await readFile(bootstrap,'utf8');
const updated=original.replace('<h2>Conversations</h2>','<h2>Conversations HMR verified</h2>');
assert.notEqual(updated,original);
const receipt=execFileSync(join(directory,'run'),['--profile','devices','register','isolated HMR'],{encoding:'utf8',timeout:30000});
const browser=await chromium.launch(),page=await browser.newPage({viewport:{width:1440,height:980}}),errors=[];
let changedSource=false,changedBootstrap=false;
page.on('pageerror',error=>errors.push(error.message));
try {
  const response=await page.goto(origin);assert.match(response.headers()['content-security-policy'],/wasm-unsafe-eval/);
  await page.getByLabel('Device registration receipt').fill(receipt.trim());await page.getByLabel('Allow local HTTP for development').check();
  await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.getByLabel('Server directory').fill(join(directory,'workspace'));
  await page.locator('#workspace-form').getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').click();
  const input=page.getByLabel('Main message',{exact:true});await input.fill('draft survives feature HMR 中文');
  const identity=await page.evaluate(()=>{window.fixtureDocumentId=crypto.randomUUID();window.fixtureEditor=document.querySelector('textarea[aria-label="Main message"]');return window.fixtureDocumentId});
  const session=await page.locator('.pane-session').textContent();
  changedSource=true;await writeFile(source,updated);
  await page.getByRole('heading',{name:'Conversations HMR verified',exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>window.fixtureDocumentId),identity);
  assert(await page.evaluate(()=>window.fixtureEditor===document.querySelector('textarea[aria-label="Main message"]')));
  assert.equal(await input.inputValue(),'draft survives feature HMR 中文');assert.equal(await page.locator('.pane-session').textContent(),session);
  const request={headers:{Origin:origin,'x-rsi-csrf':'1','x-rsi-wire-version':'1'},data:{wire_version:1}};
  const allowed=await page.request.post(origin+'/api/v1/connection/describe/1',request);assert.equal(allowed.status(),200);
  const forbidden=await page.request.post(origin+'/api/v1/connection/describe/1',{...request,headers:{...request.headers,Origin:'http://foreign.invalid'}});
  assert.equal(forbidden.status(),401);
  const missingCsrf=await page.request.post(origin+'/api/v1/connection/describe/1',{headers:{Origin:origin,'x-rsi-wire-version':'1'},data:{wire_version:1}});assert.equal(missingCsrf.status(),401);
  await page.screenshot({path:join(report,'feature-hmr.png'),fullPage:true});
  changedSource=false;await writeFile(source,original);await page.getByRole('heading',{name:'Conversations',exact:true}).waitFor();
  const navigation=page.waitForEvent('framenavigated',frame=>frame===page.mainFrame());
  changedBootstrap=true;await writeFile(bootstrap,boot+'\n// Isolated development full-reload probe.\n');await navigation;
  await page.waitForFunction(()=>document.querySelector('#login')||document.querySelector('#workbench'));
  assert.equal(await page.evaluate(()=>window.fixtureDocumentId),undefined);
  changedBootstrap=false;await writeFile(bootstrap,boot);
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),featureHmrKeptDocument:true,editorIdentity:true,draftPreserved:true,sessionPreserved:true,bootstrapFullReload:true,crossOriginMutationRejected:forbidden.status(),sameOriginApi:allowed.status(),missingCsrfRejected:missingCsrf.status()},null,2));
} catch(error) {await page.screenshot({path:join(report,'failure.png'),fullPage:true}).catch(()=>{});await writeFile(join(report,'failure.txt'),String(error));throw error}
finally {if(changedSource) await writeFile(source,original);if(changedBootstrap) await writeFile(bootstrap,boot);await browser.close()}
