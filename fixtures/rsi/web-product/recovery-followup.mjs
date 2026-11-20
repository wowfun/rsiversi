import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {verifyComposerWriterFence} from './draft-migration.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const root=fileURLToPath(new URL('../../../',import.meta.url));
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {StartingDraft} from './starting-draft.js';
fixture.seedRecovery=async()=>{
  const namespace=fixture.runtime.connection.drafts.namespace;
  for(let index=0;index<35;index++) {
    const draft=await StartingDraft.load(namespace,index===0?'compare':'draft-'+String(index).padStart(2,'0'));
    draft.edit('Retained input '+index);await draft.flush();
  }
  await fixture.runtime.panes.get('main').restoreDrafts();
};
`;
const profileSource=`
import './styles.css';
import {createRoot} from 'react-dom/client';
import {flushSync} from 'react-dom';
import {Profiler} from 'react';
import {Conversation} from './src/conversation.tsx';
import {publish} from './src/bridge.ts';
let view={surfaces:{main:{kind:'native',session:'untitled',workspace:{key:'test',coordinates:{location:{kind:'local'},path:''},registration:{status:'unregistered'}}},compare:null},catalog:{workspaces:[]}};
window.probe={commits:0,unrelated(){flushSync(()=>publish({...view,notice:'unrelated'}));},rename(){view={...view,navigation:{entries:[{session:'untitled',metadata:{title:'Renamed conversation'}}]}};flushSync(()=>publish(view));}};
publish(view);
createRoot(document.getElementById('root')).render(<Profiler id="conversation" onRender={()=>probe.commits++}><Conversation/></Profiler>);
`;
const scene=await sourceFixture({name:'recovery-followup',source});
const profiling=await sourceFixture({name:'conversation-profile',source:profileSource}),results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:980}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
    await page.goto(profiling.url);await page.locator('#pane-tab-main').waitFor();
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    const title=await page.locator('#pane-tab-main').innerText(),option=await page.locator('option[value="main"]').innerText();
    const idleCommits=await page.evaluate(()=>{const before=probe.commits;probe.unrelated();return probe.commits-before;});
    await page.evaluate(()=>probe.rename());await page.getByRole('button',{name:'Renamed conversation',exact:true}).waitFor();
    const titleUpdated=await page.locator('option[value="main"]').innerText();
    await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
    const commandStatus=await page.locator('.composer-command [role="status"]').elementHandle();
    assert.equal(await commandStatus.textContent(),'');
    const storageStatus=await page.locator('.draft-status > [role="status"]').first().elementHandle();
    assert.equal(await storageStatus.textContent(),'');
    assert.equal(await page.locator('.draft-status').evaluate(node=>node.getBoundingClientRect().height),0);
    await page.evaluate(()=>fixture.seedRecovery());
    const panel=page.locator('.draft-recovery'),first=panel.locator('.saved-draft').first();
    await page.evaluate(async()=>{
      const store=fixture.runtime.connection.drafts;
      fixture.savedDraftList=store.list;
      store.list=async()=>{throw new Error('isolated Session journal failure');};
      await fixture.runtime.panes.get('main').restoreDrafts();
    });
    const recoveryError=panel.getByText('Saved drafts could not be read: isolated Session journal failure',{exact:true});
    await recoveryError.waitFor();
    assert(await recoveryError.evaluate(node=>{
      const box=node.getBoundingClientRect(),parent=node.closest('.draft-recovery').getBoundingClientRect();
      return box.top>=parent.top && box.bottom<=parent.bottom;
    }));
    assert.equal(await panel.locator('.saved-draft').count(),32);
    await page.screenshot({path:join(report,name+'-independent-recovery.png'),fullPage:true});
    await page.evaluate(async()=>{
      fixture.runtime.connection.drafts.list=fixture.savedDraftList;
      await fixture.runtime.panes.get('main').restoreDrafts();
    });
    assert.equal(await panel.locator('.saved-draft').count(),32);
    await first.getByRole('button',{name:/^Delete starting draft:/}).click();
    await panel.getByRole('button',{name:/^(Next|Load more) starting drafts$/}).click();
    await panel.getByText('Starting draft · draft-34 · Retained input 34',{exact:true}).waitFor();
    const rowsAfterMore=await panel.locator('.saved-draft').count();
    const cancel=panel.getByRole('button',{name:/^Cancel deletion:/});
    const cancelAvailable=await cancel.count();
    if(cancelAvailable) {
      await cancel.click();assert.equal(await panel.getByRole('button',{name:/^Confirm delete starting draft:/}).count(),0);
      assert.equal(await panel.locator('.saved-draft').count(),35);
      await first.getByRole('button',{name:/^Delete starting draft:/}).click();
    }
    const firstSummary=await first.locator(':scope > span').innerText();
    const coordinates=await first.getAttribute('title');
    for(const width of [1440,430]) {
      await page.setViewportSize({width,height:980});await first.scrollIntoViewIfNeeded();
      assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      await page.screenshot({path:join(report,name+'-'+width+'.png'),fullPage:true});
    }
    await page.evaluate(()=>{
      fixture.runtime.panes.get('main').recoveryView.close();
      const pane=fixture.runtime.panes.get('main');pane.selectedCommand={name:'plan',composer_input:{kind:'text',hint:'on | off'}};
      pane.edit('/plan retained command arguments');pane.renderComposer();
    });
    const commandButtonInStatus=await page.locator('.composer-command[role="status"] button,.composer-command [role="status"] button').count();
    assert.equal(await page.getByRole('button',{name:'Leave command input',exact:true}).count(),1);
    assert.equal(await commandStatus.evaluate(node=>node===document.querySelector('.composer-command [role="status"]')),true);
    assert.match(await commandStatus.textContent(),/\/plan.*on \| off/);
    await page.screenshot({path:join(report,name+'-command.png'),fullPage:true});
    await page.getByRole('textbox',{name:/^Main (message|command arguments)$/}).fill('/export out.md');
    await page.getByRole('button',{name:'Leave command input',exact:true}).waitFor();
    assert.match(await commandStatus.textContent(),/\/export/);
    await page.getByRole('button',{name:'Leave command input',exact:true}).click();
    assert.equal(await page.getByRole('textbox',{name:'Main message',exact:true}).inputValue(),'out.md');
    await page.evaluate(async()=>{
      const pane=fixture.runtime.panes.get('main'),store=fixture.runtime.connection.drafts;
      await store.edit(await store.ensure('main','expired-unused',pane.editor.record.header),'Delete unused conversation input',[]);
      await pane.restoreDrafts();
    });
    const unused=panel.locator('.saved-draft').filter({hasText:'Delete unused conversation input'});
    await unused.getByRole('button',{name:/^Delete draft:/}).click();
    await unused.getByRole('button',{name:/^Confirm delete draft:/}).waitFor();
    await page.screenshot({path:join(report,name+'-delete-draft.png'),fullPage:true});
    await unused.getByRole('button',{name:/^Confirm delete draft:/}).click();
    await unused.waitFor({state:'hidden'});
    assert.equal(await unused.count(),0);
    await panel.getByRole('button',{name:'Discard all saved drafts…',exact:true}).click();
    await page.getByRole('button',{name:'Confirm discard all saved drafts',exact:true}).waitFor();
    await page.screenshot({path:join(report,name+'-quota-recovery.png'),fullPage:true});
    await page.evaluate(()=>{const pane=fixture.runtime.panes.get('main');pane.discardDraftsConfirmed=false;pane.renderComposer();});
    await page.evaluate(async()=>{
      await (await import('/drafts.js')).DraftStore.discardAllDrafts();
      fixture.runtime.panes.get('main').renderComposer();
    });
    await page.getByText('Draft storage is retired. Finish the reset or reload before editing.',{exact:true}).waitFor();
    assert.equal(await storageStatus.evaluate(node=>node===document.querySelector('.draft-status > [role="status"]')),true);
    assert.match(await storageStatus.textContent(),/Draft storage is retired/);
    await page.getByRole('button',{name:'Discard saved drafts…',exact:true}).click();
    await page.getByText(/including drafts that have not started a conversation/).waitFor();
    await page.screenshot({path:join(report,name+'-retired.png'),fullPage:true});
    const writerFence=await verifyComposerWriterFence(browser,root);
    const result={browser:name,title,option,idleCommits,titleUpdated,rowsAfterMore,cancelAvailable,firstSummary,coordinates,commandButtonInStatus,writerFence,errors};
    results.push(result);await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
    assert.equal(title,'New conversation');assert.equal(option,title);assert.equal(idleCommits,0);
    assert.equal(titleUpdated,'Renamed conversation');assert.equal(rowsAfterMore,35);assert.equal(cancelAvailable,1);
    assert.equal(firstSummary,'Starting draft · Compare · Retained input 0');assert.match(coordinates,/compare/);
    assert.equal(commandButtonInStatus,0);assert.deepEqual(errors,[]);
  } catch(error) {
    await page.screenshot({path:join(report,name+'-failure.png'),fullPage:true}).catch(()=>{});
    await writeFile(join(report,name+'-failure.txt'),String(error.stack));throw error;
  } finally {await browser.close();}
}} finally {await scene.close();await profiling.close();}
console.log(JSON.stringify(results));
