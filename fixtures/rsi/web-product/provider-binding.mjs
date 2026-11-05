import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {settingsPanel} from './src/bridge.ts';
fixture.openModels=()=>settingsPanel.set('models');
fixture.view.setup={ticket:'setup-1',allowed:true,credentials_available:false,providers_writable:true,settings_writable:true,agent:{default_model:null},providers:{desired_revision:'1',applied_revision:'1',applying:false,diagnostic:null,deployments:[]}};
fixture.replacements=0;fixture.refreshes=0;
fixture.setReply('command',async text=>{
 const value=JSON.parse(text),command=value.command;
 if(value.action==='setup') {
  if(command.kind==='providers_replace'){fixture.replacements++;fixture.saved=command.deployments;return '{}';}
  if(command.kind==='refresh'){fixture.refreshes++;}
  if(command.kind==='default_model'){fixture.view.setup.agent.default_model=command.model;}
 }
 await fixture.frame();return '{}';
});
fixture.status=async(revision,applying=false)=>{fixture.view.setup={...fixture.view.setup,ticket:'ticket-'+revision,providers:{...fixture.view.setup.providers,desired_revision:revision,applied_revision:applying?'1':revision,applying,deployments:fixture.saved??[]}};await fixture.frame();};
`;
const fixture=await sourceFixture({name:'provider-binding',source}),results=[];
try { for(const [name,engine] of Object.entries({chromium,firefox})) {
 const browser=await engine.launch();
 try { for(const mode of ['close','endpoint','principal','applying','superseded','finish']) {
  const page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];page.on('pageerror',error=>errors.push(error.message));
  page.setDefaultTimeout(20000);console.log(name,mode);
  try {
   await page.goto(fixture.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   await page.evaluate(()=>fixture.openModels());await page.getByRole('button',{name:'Add model provider',exact:true}).click();
   const editor=page.getByRole('form',{name:'Add model provider'});
   await editor.getByRole('button',{name:'Create provider',exact:true}).click();await page.waitForFunction(()=>fixture.replacements===1);
   if(['close','endpoint','principal'].includes(mode)) {
    await page.getByRole('button',{name:'Close settings'}).click();
    if(mode!=='close') {
     await page.evaluate(()=>fixture.transports.at(-1).onerror({preventDefault(){}}));
     await page.locator('#login').waitFor({state:'visible'});
     await page.evaluate(mode=>fixture.setReply('connect',()=>JSON.stringify({endpoint_id:(mode==='endpoint'?'b':'a').repeat(32),principal:mode==='principal'?{kind:'device',device_id:'b'.repeat(32)}:{kind:'local'}})),mode);
     await page.locator('#receipt').fill('{}');await page.getByRole('button',{name:'Connect',exact:true}).click();
     await page.locator('#workbench').waitFor({state:'visible'});
    }
    await page.evaluate(()=>fixture.status('2'));
    await page.evaluate(()=>fixture.tick());
   } else if(mode==='applying') {
    await page.evaluate(()=>fixture.status('2',true));await page.evaluate(()=>fixture.tick());
    assert.equal(await page.evaluate(()=>fixture.view.setup.agent.default_model),null);
    await page.evaluate(()=>fixture.status('2'));await editor.waitFor({state:'detached'});
   } else if(mode==='superseded') {
    await page.evaluate(()=>fixture.status('3'));await editor.getByRole('alert').filter({hasText:'changed again'}).waitFor();
   } else {
    await page.clock.install();await page.clock.fastForward(10001);
    await editor.getByRole('alert').filter({hasText:'refresh settings'}).waitFor();
    await page.evaluate(()=>fixture.status('2'));
    await editor.getByRole('button',{name:'Finish setup',exact:true}).click();await editor.waitFor({state:'detached'});
    assert.equal(await page.evaluate(()=>fixture.refreshes),1);
   }
   const trace=await page.evaluate(()=>fixture.transports.map(transport=>transport.calls.filter(call=>call.kind==='call'&&call.method==='command').map(call=>JSON.parse(call.payload)).filter(command=>command.action==='setup').map(command=>command.command.kind)));
   const defaults=trace.flat().filter(kind=>kind==='default_model');
   assert.equal(defaults.length,['applying','finish'].includes(mode)?1:0);assert.equal(await page.evaluate(()=>fixture.replacements),1);assert.deepEqual(errors,[]);
   await page.screenshot({path:join(report,name+'-'+mode+'.png')});results.push({browser:name,mode,trace,errors});
  } catch(error) {await page.screenshot({path:join(report,name+'-'+mode+'-failure.png')});throw error;} finally {await page.close();}
 } } finally {await browser.close();}
} await writeFile(join(report,'result.json'),JSON.stringify({ok:true,results},null,2));console.log(JSON.stringify({ok:true,cases:results.length}));
} finally {await fixture.close();}
