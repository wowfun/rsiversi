import {navigationMode} from './controls.mjs';
import {finishFixture} from './cleanup.mjs';
import {paired} from './paired-env.mjs';
import {startService} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {assertNoNotices} from './task-checks.mjs';
const report=process.env.RSI_REPORT_DIR;assert(report);await mkdir(report,{recursive:false});
let service,browser,page,failure;const errors=[];
try{
  service=await startService({binary:paired.binary,assets:paired.assets,report});
  browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
  await context.addInitScript(()=>{
    const Original=Worker;window.resourceReplies=[];window.resourceCalls=[];window.resourceFrames=[];
    window.Worker=class extends Original{
      constructor(...args){super(...args);this.opens=new Set();this.addEventListener('message',({data})=>{if(data.kind==='view'){const frame=JSON.parse(data.view),panels=(frame.kind==='snapshot'?frame.view:frame.sections)?.panels;if(panels){window.resourceFrames.push({frame:frame.frame_id,panels:panels.map(({view,open_revision,opening_request})=>({view,open_revision,opening_request}))});if(window.resourceFrames.length>50)window.resourceFrames.shift()}}if(data.kind==='reply'&&this.opens.delete(data.id))window.resourceReplies.push(data)})}
      postMessage(data,...rest){if(data.kind==='call'&&['resource_open','command'].includes(data.method)){const payload=JSON.parse(data.payload);if(data.method==='resource_open'||payload.action==='close_detail')window.resourceCalls.push({method:data.method,payload})}if(data.kind==='call'&&data.method==='resource_open')this.opens.add(data.id);super.postMessage(data,...rest)}
    };
  });
  page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  await connectWorkbench(page,service,'Sixteen restored tabs');await openWorkspace(page,service);
  await page.getByLabel('Main message',{exact:true}).fill('Create a durable Session for restoring resource tabs.');
  await page.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
  await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
  await page.getByRole('button',{name:'Workspace files',exact:true}).click();
  await page.locator('.resource-content .ui-contribution').waitFor();
  for(let count=1;count<16;count++){
    await page.locator('[data-dockkit-tab]').first().click({button:'right'});
    await page.getByRole('menuitem',{name:'Duplicate',exact:true}).click();
    await page.waitForFunction(expected=>document.querySelectorAll('.resource-content .ui-contribution').length===expected,count+1);
  }
  assert.equal(await page.locator('[data-dockkit-tab]').count(),16);
  await assertNoNotices(page);
  const session=await page.locator('.pane.selected').getAttribute('data-session-id');
  await page.reload();
  await page.locator('#reconnect').click();await page.locator('#workbench').waitFor({state:'visible'});
  await navigationMode(page,'flat');
  await page.locator(`[data-testid="conversation-row"][data-session-id="${session}"]`).getByTestId('conversation-open').click();
  await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===16);
  const replies=await page.evaluate(()=>window.resourceReplies);
  assert.equal(replies.length,16,JSON.stringify(replies));
  assert(replies.every(reply=>!reply.error),JSON.stringify(replies));
  assert.equal(await page.locator('[data-dockkit-tab]').count(),16);
  assert.equal(await page.locator('.resource-content .ui-contribution').count(),16);
  assert.deepEqual(await page.locator('.resource-dock [role="alert"], .resource-cleanup-notice').allTextContents(),[]);
  await assertNoNotices(page);
  await page.screenshot({path:join(report,'restored.png')});
  assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,restored:replies.length,errors},null,2));
}catch(error){
  failure=error;
  if(page)await Promise.allSettled([
    page.screenshot({path:join(report,'failure.png')}),
    page.locator('body').innerText().then(text=>writeFile(join(report,'failure.txt'),text)),
    page.evaluate(()=>({replies:window.resourceReplies,calls:window.resourceCalls,frames:window.resourceFrames})).then(evidence=>writeFile(join(report,'wire.json'),JSON.stringify(evidence))),
  ]);
  throw error;
}finally{await finishFixture(failure,()=>browser?.close(),()=>service?.close());}
