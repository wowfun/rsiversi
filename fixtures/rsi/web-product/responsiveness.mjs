import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {chooseWorkspace} from './src/directory-picker.tsx';
import {useState} from 'react';
import {createRoot} from 'react-dom/client';
import {Modal} from './src/presentation.tsx';
window.nestedDialogs=()=>{
 const container=document.body.appendChild(document.createElement('div'));
 const root=createRoot(container);
 function Nested(){
  const [child,show]=useState(false);
  return <Modal label="Parent" className="parent" close={()=>{root.unmount();container.remove()}}>
   <button disabled>Unavailable</button><button onClick={()=>show(true)}>Open child</button>
   {child&&<Modal label="Child" className="child" close={()=>show(false)}><button>Child action</button></Modal>}
  </Modal>;
 }
 root.render(<Nested/>);
};
window.highlightMany=async()=>{
 const code='const value = 7;\\n';
 const events=Array.from({length:60},()=>[{kind:'start',element:{kind:'pre',language:'js'}},{kind:'text',text:code},{kind:'end'}]).flat();
 const started=performance.now();
 fixture.view.surfaces.main.transcript.blocks[0].text=Array.from({length:60},()=>[String.fromCharCode(96).repeat(3)+'js',code,String.fromCharCode(96).repeat(3)].join('\\n')).join('\\n');
 fixture.view.surfaces.main.transcript.blocks[0].markdown=events;
 await fixture.frame();
 const highlightHost=document.querySelector('.pane.selected');
 return {elapsed:performance.now()-started,immediateTokens:highlightHost.querySelectorAll('.code-token').length,blocks:highlightHost.querySelectorAll('pre').length,acknowledgements:fixture.acknowledgements.length,phase:fixture.runtime.connection.phase};
};
window.chooseDirectory=chooseWorkspace;
`;
const fixture=await sourceFixture({name:'responsiveness-fixture',source}),results=[];
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),context=await browser.newContext({viewport:{width:900,height:900}}),page=await context.newPage(),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(fixture.url);await page.waitForFunction(()=>window.ready);
   await page.evaluate(async()=>{
    fixture.view.plugins={actions:[],target:{kind:'host'},guidance:[],ticket:'test',diagnostic:null,page:{context:{target:{kind:'host'},availability:'available'},desired_revision:'8',observed_revision:'9',offset:0,total:1,next_offset:null,plugins:[{instance:'test-plugin',desired_plugin:'example.test',enabled:true,origin:'linked',diagnostics:[],observed:{plugin:'example.test',state:'active'}}]}};
    await fixture.frame();
   });
   const checks={browser:name,version:browser.version(),navigation:[],focus:[]};
   // Observe each case before asserting, so a failing branch does not hide the others.
   for(const width of [390,767,768,900,1023]) {
    await page.setViewportSize({width,height:900});await page.evaluate(()=>fixture.tick());
    const button=page.getByRole('button',{name:width<768?'Toggle navigation':'Expand navigation',exact:true});
    if(width<768){
     const title=page.locator('#pane-tab-main');
     const [openerBox,titleBox]=await Promise.all([button.boundingBox(),title.boundingBox()]);
     assert(titleBox && openerBox && openerBox.x+openerBox.width<=titleBox.x,'mobile navigation precedes the visible conversation title');
     assert((await title.innerText()).trim());
    }
    await button.click();await page.evaluate(()=>fixture.tick());
    const drawer=page.getByRole('dialog',{name:'Workspace navigation',exact:true});
    const collapse=page.getByRole('button',{name:'Collapse navigation',exact:true});
    await collapse.waitFor();
    const modal=await drawer.isVisible();
    assert.equal(modal,width<768);
    if(modal){
     assert.equal(await button.getAttribute('aria-expanded'),'true');
     assert(await collapse.evaluate(node=>document.activeElement===node));
     await drawer.getByRole('button',{name:'Search conversations',exact:true}).click();
     const search=drawer.getByRole('textbox',{name:'Search conversations',exact:true});
     await search.fill('retained navigation');
     const original=await search.elementHandle();
     await page.mouse.click(width-5,450);
     await drawer.waitFor({state:'hidden'});
     assert(await button.evaluate(node=>node===document.activeElement));
     await button.click();await drawer.waitFor();
     assert(await search.evaluate((node,old)=>node===old,original));
     assert.equal(await search.inputValue(),'retained navigation');
     assert(await search.evaluate(node=>node===document.activeElement));
     await search.press('Escape');await search.waitFor({state:'hidden'});
     assert(await drawer.isVisible());
     await original.dispose();
    }else{
     assert.equal(await page.locator('.workbench > .sidebar').count(),1);
     await page.getByLabel('Main message',{exact:true}).focus();
     assert(await page.getByLabel('Main message',{exact:true}).evaluate(node=>node===document.activeElement));
    }
    checks.navigation.push({width,modal,expanded:true});
    await page.screenshot({path:join(report,name+'-navigation-'+width+'.png')});
    if(modal)await page.keyboard.press('Escape');else await collapse.click();
   }
   await page.setViewportSize({width:1440,height:900});await page.evaluate(()=>fixture.tick());
   const shortcuts=page.getByRole('navigation',{name:'Workspace tools'}).getByRole('button');
   assert.deepEqual((await shortcuts.allTextContents()).slice(0,2),['Plugins','Automation tasks']);
   const desktop=page.getByRole('button',{name:'Collapse navigation',exact:true});
   const peer=await page.context().newPage();
   try {
    await peer.setViewportSize({width:1440,height:900});await peer.goto(fixture.url);
    await peer.waitForFunction(()=>window.ready);await peer.evaluate(()=>fixture.frame());
    const handle=p=>p.getByRole('separator',{name:'Resize navigation',exact:true});
    const width=p=>p.locator('.workbench > .sidebar').evaluate(el=>Math.round(el.getBoundingClientRect().width));
    assert.equal(await width(page),280);assert.equal(await width(peer),280);
    await handle(page).press('ArrowRight');
    assert.equal(await width(page),290);assert.equal(await width(peer),280);
    await desktop.click();await page.getByRole('button',{name:'Expand navigation',exact:true}).waitFor();
    assert.equal(await width(peer),280);
    await page.getByRole('button',{name:'Expand navigation',exact:true}).click();assert.equal(await width(page),280);
    await handle(peer).press('ArrowLeft');assert.equal(await width(peer),270);
    await peer.reload();await peer.waitForFunction(()=>window.ready);await peer.evaluate(()=>fixture.frame());
    assert.equal(await width(peer),280);
    checks.desktopNavigation={initial:280,independentWidths:[290,280],reopened:280,reloaded:280};
   }finally{await peer.close();}
   await desktop.click();await page.evaluate(()=>fixture.tick());
   assert.equal(await desktop.count(),0);
   const rail=page.locator('.workbench > .navigation-rail');
   const plugins=rail.getByRole('button',{name:'Open plugins'});
   assert(await plugins.evaluate(button=>button.nextElementSibling?.getAttribute('aria-label')==='Automation tasks'));
   const hostReads=()=>page.evaluate(()=>fixture.calls.filter(call=>call.method==='command' && JSON.parse(call.payload).action==='plugins').length);
   const loadedReads=await hostReads();
   await plugins.click();
   const inventory=page.getByRole('region',{name:'Plugins',exact:true});
   await inventory.waitFor();assert.equal(await page.getByRole('dialog').count(),0);
   const plugin=inventory.getByRole('button',{name:'View test-plugin',exact:true});
   await plugin.click();await inventory.getByRole('heading',{name:'test-plugin',exact:true}).waitFor();
   assert(await inventory.locator('header svg use').evaluateAll(icons=>icons.length===3 &&
     icons.every(icon=>icon.getBBox().width>0 && icon.getBBox().height>0)),'all plugin header controls must draw their glyphs');
   assert.equal(await hostReads(),loadedReads,'opening a loaded Host inventory must reuse its page');
   assert(await inventory.getByText('active · example.test').isVisible());
   await inventory.getByRole('button',{name:'Back to plugins'}).click();
   await page.waitForFunction(()=>document.activeElement?.getAttribute('data-instance')==='test-plugin');
   await inventory.getByRole('button',{name:'Refresh',exact:true}).click();
   assert.equal(await hostReads(),loadedReads+1,'explicit refresh must read the Host');
   await inventory.getByRole('button',{name:'Close plugins'}).click();await inventory.waitFor({state:'hidden'});
   assert(await plugins.evaluate(button=>document.activeElement===button));
   const expand=rail.getByRole('button',{name:'Expand navigation',exact:true});
   const mark=expand.locator('.brand-logo'),panel=expand.locator('svg');
   assert.equal(await mark.count(),1,'The rail logo must be inside its single expand control');
   checks.rail=[];
   for(const theme of ['light','dark']) {
    await page.emulateMedia({colorScheme:theme});
    await page.mouse.move(700,500);
    assert(await mark.isVisible());assert.equal(await panel.isVisible(),false);
    const box=await expand.boundingBox(),logoBox=await mark.boundingBox();
    await page.screenshot({path:join(report,`${name}-rail-${theme}-rest.png`)});
    await expand.hover();
    assert.equal(await mark.isVisible(),false);assert(await panel.isVisible());
    assert.deepEqual(await expand.boundingBox(),box,'Hover must preserve the hit area');
    const panelBox=await panel.boundingBox();
    assert(Math.abs(logoBox.x+logoBox.width/2-panelBox.x-panelBox.width/2)<1);
    assert(Math.abs(logoBox.y+logoBox.height/2-panelBox.y-panelBox.height/2)<1);
    await page.getByRole('tooltip',{name:'Expand navigation',exact:true}).waitFor();
    await page.screenshot({path:join(report,`${name}-rail-${theme}-hover.png`)});
    checks.rail.push({theme,box,hover:true});
   }
   await page.mouse.move(700,500);await expand.focus();
   await page.keyboard.press('Tab');await page.keyboard.press('Shift+Tab');
   assert(await expand.evaluate(node=>node===document.activeElement));
   assert(await expand.evaluate(node=>node.matches(':focus-visible')));
   assert.equal(await mark.isVisible(),false);assert(await panel.isVisible());
   await expand.press('Enter');await page.evaluate(()=>fixture.tick());
   assert(await desktop.isVisible());
   await desktop.click();await page.evaluate(()=>fixture.tick());
   const search=page.getByRole('textbox',{name:'Search conversations',exact:true});
   const railSearch=rail.getByRole('button',{name:'Search conversations',exact:true});
   const verifySearchFocus=async()=>{
    await search.waitFor();
    await page.waitForFunction(()=>document.activeElement?.getAttribute('aria-label')==='Search conversations' && document.activeElement.tagName==='INPUT');
    assert.equal(await page.getByRole('dialog',{name:'Command palette',exact:true}).count(),0);
   };
   await railSearch.hover();
   const searchTip=page.getByRole('tooltip',{name:/^Search conversations/});
   await searchTip.waitFor();assert.deepEqual(await searchTip.locator('kbd').allTextContents(),['Ctrl','+','Alt','+','K']);
   await railSearch.click();await verifySearchFocus();
   assert(await desktop.isVisible());
   await search.fill('retained rail query');
   const composer=page.getByLabel('Main message',{exact:true});
   await composer.focus();await page.keyboard.press('Control+Alt+k');await verifySearchFocus();
   assert.equal(await search.inputValue(),'retained rail query');
   await page.screenshot({path:join(report,name+'-rail-search.png')});
   await search.press('Escape');await search.waitFor({state:'hidden'});
   await page.setViewportSize({width:900,height:900});await page.evaluate(()=>fixture.tick());
   await railSearch.click();await verifySearchFocus();await desktop.waitFor();
   await composer.click();await search.waitFor({state:'hidden'});await desktop.click();
   await page.setViewportSize({width:390,height:900});await page.evaluate(()=>fixture.tick());
   await composer.focus();await page.keyboard.press('Control+Alt+k');await verifySearchFocus();
   const narrowDrawer=page.getByRole('dialog',{name:'Workspace navigation',exact:true});
   assert(await narrowDrawer.isVisible());
   await page.screenshot({path:join(report,name+'-drawer-search.png')});
   await page.keyboard.press('Escape');await search.waitFor({state:'hidden'});
   assert(await narrowDrawer.isVisible());
   await page.keyboard.press('Escape');await narrowDrawer.waitFor({state:'hidden'});
   checks.railSearch={desktopClick:true,refocusedQuery:true,tabletOutsideDismiss:true,mobileKeyboard:true,escapeClosesInOrder:true};
   await page.setViewportSize({width:1440,height:900});await page.evaluate(()=>fixture.tick());
   const commands=page.getByRole('button',{name:'Commands',exact:true});
   for(const close of ['escape','button']) {
    await commands.focus();await commands.press('Enter');
    await page.getByRole('dialog',{name:'Command palette',exact:true}).waitFor();
    const entered=await page.getByLabel('Search commands',{exact:true}).evaluate(node=>node===document.activeElement);
    if(close==='escape')await page.keyboard.press('Escape');else await page.getByLabel('Close commands',{exact:true}).click();
    await page.evaluate(()=>fixture.tick());
    checks.focus.push({modal:'palette',close,entered,restored:await commands.evaluate(node=>node===document.activeElement)});
   }
   const message=page.getByLabel('Main message',{exact:true});
   await message.focus();await page.keyboard.press('Control+k');
   await page.getByRole('dialog',{name:'Command palette',exact:true}).waitFor();
   const keyboardEntered=await page.getByLabel('Search commands',{exact:true}).evaluate(node=>node===document.activeElement);
   await page.keyboard.press('Control+Alt+k');
   assert(await page.getByLabel('Search commands',{exact:true}).evaluate(node=>node===document.activeElement));
   assert.equal(await search.count(),0);
   await page.keyboard.press('Escape');await page.evaluate(()=>fixture.tick());
   checks.focus.push({modal:'palette-keyboard',entered:keyboardEntered,restored:await message.evaluate(node=>node===document.activeElement)});
   await commands.focus();await page.evaluate(()=>window.chooseDirectory());
   await page.getByRole('dialog',{name:'Select workspace directory',exact:true}).waitFor();
   const directoryEntered=await page.getByLabel('Directory path',{exact:true}).evaluate(node=>node===document.activeElement);
   await page.keyboard.press('Escape');await page.evaluate(()=>fixture.tick());
   checks.focus.push({modal:'directory',entered:directoryEntered,restored:await commands.evaluate(node=>node===document.activeElement)});
   await page.evaluate(()=>window.nestedDialogs());
   const childOpener=page.getByRole('button',{name:'Open child',exact:true});
   await childOpener.press('Enter');
   const childAction=page.getByRole('button',{name:'Child action',exact:true});
   await childAction.waitFor();
   for(const key of ['Tab','Shift+Tab']){
    await page.keyboard.press(key);
    assert(await childAction.evaluate(el=>el===document.activeElement));
   }
   await page.keyboard.press('Escape');
   await page.getByRole('dialog',{name:'Child',exact:true}).waitFor({state:'hidden'});
   assert(await childOpener.evaluate(el=>el===document.activeElement),'closing the child returns to the still-open parent');
   await page.keyboard.press('Escape');await page.getByRole('dialog',{name:'Parent',exact:true}).waitFor({state:'hidden'});
   checks.highlight=await page.evaluate(()=>window.highlightMany());
   await message.fill('Draft remains intact while highlighting');
   await page.locator('.code-token').first().waitFor();
   assert.equal(await message.inputValue(),'Draft remains intact while highlighting');
   await page.screenshot({path:join(report,name+'-highlight.png')});
   const touch=await browser.newPage({viewport:{width:900,height:900},hasTouch:true});
   try {
    await touch.goto(fixture.url);await touch.waitForFunction(()=>window.ready);await touch.evaluate(()=>fixture.frame());
    assert.equal(await touch.evaluate(()=>matchMedia('(hover: hover)').matches),false);
    const touchExpand=touch.getByRole('button',{name:'Expand navigation',exact:true});
    assert(await touchExpand.locator('.brand-logo').isVisible());
    await touchExpand.tap();
    await touch.getByRole('button',{name:'Collapse navigation',exact:true}).waitFor();
    assert.equal(await touch.getByRole('dialog',{name:'Workspace navigation',exact:true}).count(),0);
    checks.touch={logoTapExpandedColumn:true};
   } finally {await touch.close()}
   checks.errors=errors;results.push(checks);
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
 for(const result of results) {
  assert(result.navigation.every(item=>item.expanded && item.modal===(item.width<768)),JSON.stringify(result.navigation));
  assert(result.focus.every(item=>item.entered&&item.restored),JSON.stringify(result.focus));
  assert.equal(result.highlight.blocks,60);assert.equal(result.highlight.immediateTokens,0,'Tokenization must not run in the React commit');
  assert.equal(result.highlight.acknowledgements,2);assert.equal(result.highlight.phase,'Ready');
  assert.deepEqual(result.errors,[]);
 }
} finally {await fixture.close()}
