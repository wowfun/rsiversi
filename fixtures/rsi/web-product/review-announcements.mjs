import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8');
const scene=await sourceFixture({name:'review-announcements',source});
const evidence=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1280,height:900}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
    await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
    await page.evaluate(()=>fixture.surface('restored-session',{projections:{cursor:{kind:'live'},entries:[],generation_rejection:'Checkpoint refused'}}));
    const live=page.locator('.pane.selected .sr-only[role="status"]');
    await live.getByText('Plugin update failed; previous plugins remain in use. Checkpoint refused',{exact:true}).waitFor({timeout:5000});
    const identity=await live.locator('span').elementHandle();
    await page.evaluate(()=>fixture.frame());
    assert(await identity.evaluate(node=>node.isConnected),'unchanged rejection must not remount its announcement');
    await page.evaluate(()=>fixture.surface('restored-session'));
    assert.equal(await live.textContent(),'','clearing a failure in the same Session must clear the live region');
    await page.evaluate(()=>fixture.surface('clean-session'));
    assert.equal(await live.textContent(),'');
    await page.evaluate(async()=>{
      const pane=fixture.runtime.panes.get('main');await pane.flush();
      pane.editor.failure=Object.assign(new Error('Fixture storage quota exceeded'),{resolve:async()=>{}});
      pane.renderComposer();
    });
    const failure=page.locator('.draft-error[role="status"]');
    await failure.getByText('Input is not saved. Fixture storage quota exceeded',{exact:true}).waitFor();
    for(const theme of ['light','dark']) {
      await page.emulateMedia({colorScheme:theme});
      const colors=await failure.evaluate(node=>{
        const s=getComputedStyle(node);let parent=node,bg;
        while(parent){const color=getComputedStyle(parent).backgroundColor;if(color!=='rgba(0, 0, 0, 0)'){bg=color;break;}parent=parent.parentElement;}
        return {color:s.color,background:bg,weight:Number(s.fontWeight),height:node.getBoundingClientRect().height};
      });
      const luminance=color=>{const values=color.match(/[\d.]+/g).slice(0,3).map(v=>{const n=Number(v)/255;return n<=.04045?n/12.92:((n+.055)/1.055)**2.4});return values[0]*.2126+values[1]*.7152+values[2]*.0722};
      const a=luminance(colors.color),b=luminance(colors.background),contrast=(Math.max(a,b)+.05)/(Math.min(a,b)+.05);
      assert(colors.weight>=600&&colors.height>0,JSON.stringify(colors));assert(contrast>=4.5,JSON.stringify({colors,contrast}));
      await page.screenshot({path:join(report,`${name}-${theme}.png`)});evidence.push({browser:name,theme,colors,contrast});
    }
    assert.deepEqual(errors,[]);
  }catch(error){await writeFile(join(report,`${name}-errors.json`),JSON.stringify({errors,body:await page.locator('body').innerText()},null,2));await page.screenshot({path:join(report,`${name}-failure.png`)});throw error;}
  finally{await browser.close();}
}}finally{await scene.close();}
await writeFile(join(report,'result.json'),JSON.stringify(evidence,null,2));console.log(JSON.stringify(evidence));
