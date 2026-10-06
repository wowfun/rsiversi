import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,chmod} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
const report=resolve(process.env.RSI_AUTOMATION_WEB_REPORT),seed=resolve(process.env.RSI_AUTOMATION_WEB_SEED);
await mkdir(report,{recursive:false});let receipt;
const service=await startService({binary:resolve(process.env.RSI_WEB_BINARY),assets:resolve(process.env.RSI_WEB_ASSETS),report,configure:async({config,run})=>{
 const runtime=JSON.parse(await readFile(join(seed,'runtime-config.json'))),host=join(config,'host-profiles/fixture/host.profile.toml');
 await writeFile(host,(await readFile(host,'utf8'))+'\n[[steps]]\nkind="patch"\ntarget="automation"\nconfig_json='+JSON.stringify(JSON.stringify({directory:seed,runtime}))+'\n[[steps]]\nkind="patch"\ntarget="automation"\nenabled=true\n');
}});
try {
 receipt=service.register("Automation visual viewer");
 await service.restart(async()=>{const policyPath=join(seed,'policy/policy.json'),policy=JSON.parse(await readFile(policyPath));policy.grants=[{device:receipt.id,source:'visual',rule:'visual',view:true,cancel:true,resume:true}];await writeFile(policyPath,JSON.stringify(policy));await chmod(policyPath,0o600);});
} catch(error) { await service.close(); throw error; }
const browser=await chromium.launch({headless:true});
try{
 const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.addInitScript(()=>{
  const NativeWorker=window.Worker;let projection;
  window.Worker=class extends NativeWorker {
   set onmessage(callback){super.onmessage=event=>{
    const data=event.data;
    if(data.kind==='view'){
     let frame=typeof data.view==='string'?JSON.parse(data.view):data.view;
     projection=frame.kind==='snapshot'?frame.view:{...projection,...frame.sections};
     if(window.fixtureMissingSurface&&projection?.automation?.attempt){
      projection={...projection,surfaces:{},automation:{...projection.automation,attempt:{...projection.automation.attempt,session_id:'presentation-fault-session'}}};
      frame={kind:'snapshot',frame_id:frame.frame_id,view:projection};window.fixtureInjectedSession=true;
      data.view=typeof data.view==='string'?JSON.stringify(frame):frame;
     }
    }
    callback(event);
   };}
   get onmessage(){return super.onmessage;}
  };
 });
 await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(receipt));await page.getByRole('button',{name:'Connect',exact:true}).click();
 await page.getByRole('button',{name:'Deployment checks',exact:true}).click();await page.locator('.automation-row').first().click();
 await page.locator('.automation-detail').getByText('Expected visible text is missing').waitFor();await page.getByRole('button',{name:'Read screenshot',exact:true}).click();
 await page.waitForFunction(()=>{const i=document.querySelector('.automation-screenshot');return i?.complete&&i.naturalWidth===1280});
 for(const [name,width,height,theme] of [['wide-light',1440,980,'light'],['narrow-light',390,844,'light'],['narrow-dark',390,844,'dark']]){
  await page.setViewportSize({width,height});await page.emulateMedia({colorScheme:theme});await page.screenshot({path:join(report,name+'.png')});
  assert(await page.locator('.automation-panel').evaluate(e=>e.scrollWidth<=e.clientWidth+1),'panel horizontal overflow');
  assert(await page.locator('.automation-controls').getByRole('button',{name:'New attempt',exact:true}).isVisible());
 }
 await page.setViewportSize({width:1440,height:980});await page.getByRole('button',{name:'New attempt',exact:true}).click();await page.getByText('Attempt 2: queued.',{exact:false}).waitFor();await page.getByRole('button',{name:'Read attempt',exact:true}).click();
 await page.locator('.automation-detail h3').getByText('Attempt 2',{exact:true}).waitFor();
 await page.getByRole('button',{name:'Refresh',exact:true}).click();await page.locator('.automation-row').first().click();await page.locator('.automation-detail h3').getByText('Attempt 1',{exact:true}).waitFor();assert(await page.locator('.automation-detail').getByText('Expected visible text is missing').isVisible());
 // This injected link exercises document presentation only; it grants no Session authority.
 await page.evaluate(()=>{window.fixtureMissingSurface=true;});
 await page.getByRole('button',{name:'Refresh',exact:true}).click();await page.locator('.automation-row').first().click();
 assert.equal(await page.locator('.pane').count(),0,'injected missing-surface projection must have no conversation pane');
 await page.getByRole('button',{name:'Open investigation',exact:true}).click();
 const localError=page.locator('.automation-panel [role=alert]');await localError.getByText('Open a conversation surface first',{exact:true}).waitFor();
 assert(await page.evaluate(()=>window.fixtureInjectedSession===true));
 await page.screenshot({path:join(report,'open-investigation-no-surface.png')});
 assert.deepEqual(errors,[]);await writeFile(join(report,'report.json'),JSON.stringify({ok:true,actualWorkerApi:true,deviceGrant:true,newAttempt:'2',retainedOriginal:true,presentationFault:{injectedSessionLink:true,injectedEmptySurfaces:true,noConversationSurface:true,panelError:await localError.textContent()},views:['wide-light','narrow-light','narrow-dark']},null,2));
 await context.close();
}finally{await browser.close();await service.close();}
