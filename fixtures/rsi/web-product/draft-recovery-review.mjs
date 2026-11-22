import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {verifyDraftMigration,verifyDraftReset,verifyDraftUpgradeErrors} from './draft-migration.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const root=fileURLToPath(new URL('../../../',import.meta.url));
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.recovery=state=>{const pane=fixture.runtime.panes.get('main');pane.recoveryView={close(){pane.recoveryView=undefined;pane.invalidate();},...state};pane.invalidate();};
fixture.savedText='x'.repeat(4095)+'🚀'+'full input survives preview '.repeat(30000);
fixture.showSaved=()=>fixture.recovery({busy:false,loading:false,rows:Array.from({length:4},(_,i)=>({id:String(i),summary:'Starting draft '+i,text:fixture.savedText,starting:true,empty:false,async open(){fixture.opened=fixture.savedText;},async remove(){}}))});
`;
const scene=await sourceFixture({name:'draft-recovery-review',source}),results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:980}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
    await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
    await page.evaluate(()=>fixture.recovery({rows:[],busy:true,loading:true}));
    const panel=page.locator('.draft-recovery');await panel.waitFor();
    const observed={loadingAnnounced:await panel.getByText('Loading saved drafts…',{exact:true}).count(),loadingEmpty:await panel.getByText('No saved drafts',{exact:true}).count()};
    await page.evaluate(()=>fixture.recovery({rows:[],busy:false,loading:false,notice:'Starting drafts could not be read: validation refused'}));
    observed.failedEmpty=await panel.getByText('No saved drafts',{exact:true}).count();
    await page.screenshot({path:join(report,name+'-failure-state.png')});
    await page.evaluate(()=>fixture.showSaved());await panel.locator('textarea').first().waitFor();
    observed.previewUnits=await panel.locator('textarea').evaluateAll(nodes=>nodes.reduce((sum,node)=>sum+node.value.length,0));
    observed.expand=await panel.getByRole('button',{name:'Show full input: Starting draft 0',exact:true}).count();
    observed.deleteLabel=await panel.getByRole('button',{name:'Delete starting draft: Starting draft 0',exact:true}).count();
    observed.openLabel=await panel.getByRole('button',{name:'Start a new conversation here: Starting draft 0',exact:true}).count();
    for(let index=0;index<4;index++) assert.equal(await panel.getByRole('textbox',{name:`Saved starting input: Starting draft ${index}`,exact:true}).count(),1);
    assert.deepEqual(observed,{loadingAnnounced:1,loadingEmpty:0,failedEmpty:0,previewUnits:4095*4,expand:1,deleteLabel:1,openLabel:1});
    await panel.getByRole('button',{name:'Show full input: Starting draft 0',exact:true}).click();
    assert.equal(await panel.locator('textarea').first().inputValue(),await page.evaluate(()=>fixture.savedText));
    await panel.getByRole('button',{name:'Collapse input: Starting draft 0',exact:true}).click();
    await panel.getByRole('button',{name:'Start a new conversation here: Starting draft 0',exact:true}).click();
    assert.equal(await page.evaluate(()=>fixture.opened===fixture.savedText),true);
    for(const width of [1440,430]) {
      await page.setViewportSize({width,height:980});
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      await page.screenshot({path:join(report,name+'-'+width+'.png'),fullPage:true});
    }
    await page.evaluate(()=>fixture.recovery({busy:false,rows:[
      {id:'session',summary:'Session note',text:'Saved text',removable:true},
      {id:'expired',summary:'Expired note',text:'Keep this text',expired:true,removable:true},
      {id:'conflict',summary:'Edited note',text:'From storage',expired:true,unsaved:{text:'From this window',message:'Save failed'}},
    ]}));
    await panel.getByText('Session note',{exact:true}).waitFor();
    const previews=await panel.getByRole('textbox').evaluateAll(nodes=>nodes.map(node=>[node.getAttribute('aria-label'),node.value]));
    assert.deepEqual(previews,[
      ['Saved conversation input: Session note','Saved text'],
      ['Expired conversation input: Expired note','Keep this text'],
      ['Expired conversation input: Edited note','From storage'],
      ['Unsaved recovered input: Edited note','From this window'],
    ]);
    await panel.getByText('Saved input',{exact:true}).waitFor();
    await panel.getByText('Input in this window',{exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:join(report,name+'-recovery-choices.png')});
    await page.evaluate(()=>{
      const pane=fixture.runtime.panes.get('main');pane.selectedCommand={name:'plan',composer_input:{kind:'text',hint:'on | off'}};
      pane.edit('/plan retained command arguments');pane.switching=true;pane.renderComposer();
    });
    assert.equal(await page.getByRole('button',{name:'Leave command input',exact:true}).isDisabled(),true);
    const migrations=await verifyDraftMigration(browser,root);
    const reset=await verifyDraftReset(browser,root);
    const upgradeErrors=await verifyDraftUpgradeErrors(browser,root);
    assert.deepEqual(errors,[]);results.push({browser:name,observed,fullInputRetained:true,migrations,reset,upgradeErrors,errors});
  } catch(error) {
    await page.screenshot({path:join(report,name+'-failure.png'),fullPage:true}).catch(()=>{});
    await writeFile(join(report,name+'-failure.txt'),String(error.stack));throw error;
  } finally {await browser.close();}
}} finally {await scene.close();}
await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
