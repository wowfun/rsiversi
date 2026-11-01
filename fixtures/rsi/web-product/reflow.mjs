import assert from 'node:assert/strict';
import {mkdir, readFile, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8');
const server=await sourceFixture({name:'reflow',source});
const geometry=()=>({width:innerWidth,overflow:document.documentElement.scrollWidth-innerWidth,
  textSize:getComputedStyle(document.querySelector('.message-text')??document.body).fontSize,
  clipped:[...document.querySelectorAll('button,input,textarea,select,summary')].filter(node=>{
    const box=node.getBoundingClientRect();return box.width&&box.height&&getComputedStyle(node).visibility!=='hidden'&&(box.left < -1 || box.right > innerWidth+1);
  }).map(node=>node.getAttribute('aria-label')||node.textContent.slice(0,80))});
try {
  for(const [name,engine] of Object.entries({chromium,firefox})) {
    const browser=await engine.launch(),page=await browser.newPage({viewport:{width:390,height:844}}),checks=[],errors=[];
    page.setDefaultTimeout(15000);page.on('pageerror',error=>errors.push(error.message));
    try {
      await page.goto(server.url,{timeout:90000});await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
      for(const width of [320,390])for(const size of [12,14,17])for(const mode of ['light','dark','forced']) {
        await page.setViewportSize({width,height:844});
        await page.emulateMedia({colorScheme:mode==='dark'?'dark':'light',forcedColors:mode==='forced'?'active':'none',reducedMotion:'reduce'});
        await page.evaluate(async({size,mode})=>{fixture.view.appearance={theme:mode==='dark'?'dark':'light',content_font_size:size};await fixture.frame();await fixture.tick();},{size,mode});
        const input=page.getByLabel('Main message',{exact:true});await input.focus();await input.press('Tab');
        const focused=await page.evaluate(()=>{const e=document.activeElement,r=e.getBoundingClientRect(),s=getComputedStyle(e);return {tag:e.tagName,visible:r.width>0&&r.height>0,outline:s.outlineStyle,border:s.borderStyle};});
        assert(focused.visible&&focused.tag!=='BODY',JSON.stringify(focused));
        const navigation=page.getByRole('button',{name:'Toggle navigation',exact:true});await navigation.focus();await navigation.press('Enter');
        await page.getByRole('dialog',{name:'Workspace navigation',exact:true}).waitFor();await page.keyboard.press('Escape');
        assert(await navigation.evaluate(node=>node===document.activeElement),'navigation restores focus');
        const result=await page.evaluate(geometry);assert.equal(result.textSize,`${size}px`);assert(result.overflow<=1,JSON.stringify({width,size,mode,...result}));
        assert.deepEqual(result.clipped,[],JSON.stringify({width,size,mode,...result}));
        checks.push({width,size,mode,focused,...result});
        if(size===17)await page.screenshot({path:join(report,`${name}-${width}-${mode}-17.png`)});
      }
      await page.evaluate(()=>{const node=document.createElement('div');node.id='overflow-negative-control';node.style.cssText='position:absolute;left:0;top:0;width:2000px;height:1px';document.body.append(node);});
      assert((await page.evaluate(geometry)).overflow>1,'overflow probe must detect negative control');
      await page.locator('#overflow-negative-control').evaluate(node=>node.remove());
      // Real external permission owner, with its actual command payload.
      await page.evaluate(async()=>{
        fixture.view.surfaces.main={kind:'external',generation:'external-1',external:{conversation:{kind:'external',id:'peer'},following:true,more:false,busy:false,diagnostic:null,blocks:[],capabilities:{goal:false,preset:false,submit:true,load:true,resume:true},observed:{snapshot:{id:'peer',endpoint:'fixture',cwd:'/fixture',status:'idle',completion:null,generation:'1',epoch:'1'},connected:true,permissions:[{id:'permission-1',generation:'1',title:'Allow read of the selected file?',options:[{id:'allow',name:'Allow once',kind:'allow_once'},{id:'deny',name:'Deny',kind:'reject_once'}]}]}}};
        await fixture.frame();
      });
      const allow=page.getByRole('button',{name:'Allow once · allow once',exact:true});await allow.focus();await allow.press('Enter');
      await page.waitForFunction(()=>fixture.calls.some(call=>call.method==='command'&&call.payload.includes('permission-1')));
      await page.screenshot({path:join(report,`${name}-permission.png`)});
      await page.evaluate(async()=>{fixture.view.surfaces.main.external.busy=true;await fixture.frame();});assert(await allow.isDisabled());
      await page.evaluate(async()=>{fixture.view.surfaces.main.external.busy=false;await fixture.frame();});assert(await allow.isEnabled());
      await page.evaluate(()=>fixture.surface('terminal-session'));
      const options=page.getByLabel('Conversation options',{exact:true});await options.focus();await options.press('Enter');
      const terminal=page.getByRole('button',{name:'Terminal',exact:true});await terminal.focus();await terminal.press('Enter');
      const refresh=page.getByRole('button',{name:'Refresh terminals',exact:true});await refresh.waitFor();await refresh.focus();await refresh.press('Enter');
      await page.waitForFunction(()=>fixture.calls.some(call=>call.method==='terminal'&&JSON.parse(call.payload).request.type==='list'));
      await page.screenshot({path:join(report,`${name}-terminal-controls.png`)});
      await page.evaluate(()=>fixture.runtime.connection.fail(new Error('Injected connection failure')));
      await page.getByText(/Injected connection failure/).first().waitFor();
      const connect=page.getByRole('button',{name:'Connect',exact:true});await connect.waitFor();
      assert(await connect.isEnabled());
      await page.getByLabel('Device registration receipt').fill('{}');await connect.focus();await connect.press('Enter');
      await page.waitForFunction(()=>fixture.runtime.connection.phase==='Ready');await page.evaluate(()=>fixture.frame());
      await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
      assert.deepEqual(errors,[]);
      await writeFile(join(report,`${name}.json`),JSON.stringify({status:'passed',browser:browser.version(),checks,negativeControl:true,permissionKeyboard:true,terminalKeyboard:true,reconnectKeyboard:true,errors},null,2));
    } catch(error) {
      await page.screenshot({path:join(report,`${name}-failure.png`)});
      await writeFile(join(report,`${name}-failure.json`),JSON.stringify({error:String(error),checks,errors},null,2));throw error;
    } finally {await browser.close();}
  }
} finally {await server.close();}
