import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);
await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {chooseWorkspace} from './src/directory-picker.tsx';
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
  const browser=await engine.launch();const page=await browser.newPage({viewport:{width:900,height:900}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(fixture.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   const checks={browser:name,version:browser.version(),navigation:[],focus:[]};
   // Observe each case before asserting, so a failing branch does not hide the others.
   for(const width of [767,768,900,1023]) {
    await page.setViewportSize({width,height:900});await page.evaluate(()=>fixture.tick());
    const button=page.getByRole('button',{name:width<768?'Toggle navigation':'Expand navigation',exact:true});
    await button.click();await page.evaluate(()=>fixture.tick());
    const opened=await page.getByRole('dialog',{name:'Workspace navigation',exact:true}).isVisible();
    checks.navigation.push({width,opened,expanded:await button.getAttribute('aria-expanded')});
    await page.screenshot({path:join(report,name+'-navigation-'+width+'.png')});
    if(opened)await page.keyboard.press('Escape');
   }
   await page.setViewportSize({width:1440,height:900});await page.evaluate(()=>fixture.tick());
   const desktop=page.getByRole('button',{name:'Collapse navigation',exact:true});
   await desktop.click();await page.evaluate(()=>fixture.tick());
   assert.equal(await desktop.count(),0);
   const rail=page.locator('.workbench > .navigation-rail');
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
   await expand.click();await page.evaluate(()=>fixture.tick());
   assert(await desktop.isVisible());
   await page.setViewportSize({width:900,height:900});await page.evaluate(()=>fixture.tick());
   await expand.click();await page.getByRole('dialog',{name:'Workspace navigation',exact:true}).waitFor();
   await page.keyboard.press('Escape');
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
   await page.keyboard.press('Escape');await page.evaluate(()=>fixture.tick());
   checks.focus.push({modal:'palette-keyboard',entered:keyboardEntered,restored:await message.evaluate(node=>node===document.activeElement)});
   await commands.focus();await page.evaluate(()=>window.chooseDirectory());
   await page.getByRole('dialog',{name:'Select workspace directory',exact:true}).waitFor();
   const directoryEntered=await page.getByLabel('Directory path',{exact:true}).evaluate(node=>node===document.activeElement);
   await page.keyboard.press('Escape');await page.evaluate(()=>fixture.tick());
   checks.focus.push({modal:'directory',entered:directoryEntered,restored:await commands.evaluate(node=>node===document.activeElement)});
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
    await touch.getByRole('dialog',{name:'Workspace navigation',exact:true}).waitFor();
    checks.touch={logoTapOpenedDrawer:true};
   } finally {await touch.close()}
   checks.errors=errors;results.push(checks);
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
 for(const result of results) {
  assert(result.navigation.every(item=>item.opened&&item.expanded==='true'),JSON.stringify(result.navigation));
  assert(result.focus.every(item=>item.entered&&item.restored),JSON.stringify(result.focus));
  assert.equal(result.highlight.blocks,60);assert.equal(result.highlight.immediateTokens,0,'Tokenization must not run in the React commit');
  assert.equal(result.highlight.acknowledgements,2);assert.equal(result.highlight.phase,'Ready');
  assert.deepEqual(result.errors,[]);
 }
} finally {await fixture.close()}
