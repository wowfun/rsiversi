import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
import {verifyTerminals} from './terminals.mjs';
import {verifyPluginSources} from './plugins.mjs';
const report=process.env.RSI_WEB_REPORT,assets=process.env.RSI_WEB_ASSETS,binary=process.env.RSI_WEB_BINARY;
assert(report&&assets&&binary,'explicit isolated inputs required');await mkdir(report,{recursive:true});
for(const[name,engine]of[['chromium',chromium],['firefox',firefox]]){
 if(process.env.RSI_WEB_BROWSER&&process.env.RSI_WEB_BROWSER!==name)continue;
 const directory=join(report,name);await mkdir(directory,{recursive:true});
 const service=await startService({binary,assets,report:directory}),browser=await engine.launch();let page;const errors=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30000);page.on('pageerror',error=>errors.push(error.message));
  await page.addInitScript(()=>{window.terminalStyleViolations=[];document.addEventListener('securitypolicyviolation',event=>{if(event.effectiveDirective.startsWith('style-src'))window.terminalStyleViolations.push({directive:event.effectiveDirective,source:event.sourceFile,line:event.lineNumber})})});
  await page.addInitScript(()=>{
    window.terminalWire={pages:0,blockedReads:0,forbidden:0};
    const NativeWorker=window.Worker;
    window.Worker=class extends NativeWorker {
      constructor(...args){super(...args);this.addEventListener('message',({data})=>{
        if(data.kind!=='reply'||typeof data.result!=='string')return;
        let reply;try{reply=JSON.parse(data.result)}catch{return}
        if(reply.type==='output'){
          window.terminalWire.pages++;
          if(/\x1b[\]P_^X]/.test(reply.value.text))window.terminalWire.forbidden++;
        }
      })}
      postMessage(data,...rest){
        if(data.method==='terminal'&&JSON.parse(data.payload).request.type==='read'&&window.terminalWire.blockedReads<12){
          window.terminalWire.blockedReads++;
          queueMicrotask(()=>this.dispatchEvent(new MessageEvent('message',{data:{kind:'reply',id:data.id,error:'fixture admission pressure',notAdmitted:true}})));
          return;
        }
        super.postMessage(data,...rest);
      }
    };
  });
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} terminal verification`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.locator('#workspace-path').fill(service.workspace);await page.getByRole('button',{name:'Add workspace',exact:true}).click();await page.locator('#workspaces .nav-item').first().click();
  const pane=page.locator('[aria-label="Main conversation"]');await pane.getByRole('textbox',{name:'Main message'}).fill('Publish the isolated terminal Session');await pane.getByRole('button',{name:'Send ↗',exact:true}).click();await pane.locator('.pane-status').filter({hasText:'Completed'}).waitFor();
  const requests=service.provider.requests.length;await verifyTerminals(page,service,directory,name);assert.equal(service.provider.requests.length,requests,'terminal operation invoked the model');
  assert.deepEqual(await page.evaluate(()=>window.terminalStyleViolations),[],'terminal blocked by product CSP');
  await verifyPluginSources(page,service,directory,name);
  await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});assert.deepEqual(errors,[]);await writeFile(join(directory,'result.json'),JSON.stringify({status:'passed',browser:browser.version(),terminal_model_requests:0},null,2));
 }catch(error){await page?.screenshot({path:join(directory,'failure.png')});await writeFile(join(directory,'failure.txt'),String(error));throw error;}
 finally{await browser.close();await service.close();}
}
