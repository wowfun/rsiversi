import assert from 'node:assert/strict';
import test from 'node:test';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

test('the Chat panel follows its transcript gutter and the attached folder', async t => {
  const fixture = await sourceFixture({name:'session-environment-fixture',source:`
    import React from 'react';
    import {createRoot} from 'react-dom/client';
    import {SessionPanel} from '/src/session/session-panel.tsx';
    import {IconSprite} from '/src/icon-sprite.tsx';
    import {installActions,notices} from '/src/bridge.ts';
    import {presentationIdentity} from '/src/presentation.tsx';
    import {setClipboardWriter} from '/mounts.js';
    window.copyResult=()=>({writes:window.writes, notice:notices.getSnapshot().transient});
    window.writes=[];
    window.ownerChanged=()=>{window.hostOwner=true;presentationIdentity.set('owner');};
    window.folderRequests=[];
    installActions({call:async (method,payload)=>{
      const request=JSON.parse(payload);window.folderRequests.push(request);
      return JSON.stringify({status:'ok',value:request.action==='desktop'?window.hostOwner===true:null});
    }});
    setClipboardWriter(async text=>{
      if(window.rejectCopy) throw new Error('Clipboard permission denied');
      window.writes.push(text);
    });
    document.body.style.margin='0';
    document.body.style.setProperty('--dsw-menu-backdrop-filter','blur(14px)');
    const root=createRoot(document.querySelector('#root'));
    window.show=(path='/home/work/project',chat=true,local=true)=>root.render(<><IconSprite/>
      <header className="pane-header"><div className="pane-heading"><SessionPanel key={path} path={path} local={local} chat={chat}/></div></header>
      <button style={{marginTop:'160px'}}>Outside</button></>);
    window.show();
  `});
  t.after(()=>fixture.close());
  for(const [name,engine] of Object.entries({chromium,firefox})) {
    await t.test(name,async t=>{
      const browser=await engine.launch();t.after(()=>browser.close());
      const page=await browser.newPage({viewport:{width:1575,height:880}});
      await page.goto(fixture.url);
      const trigger=page.getByRole('button',{name:'Session panel',exact:true});
      const panel=page.getByRole('complementary',{name:'Session panel',exact:true});
      await trigger.waitFor();assert.equal(await trigger.getAttribute('aria-expanded'),'false');
      await trigger.press('Enter');await panel.waitFor();
      assert.equal(await panel.getAttribute('data-presentation'),'overlay');
      const folder=panel.getByLabel('Working folder: /home/work/project',{exact:true});
      await folder.focus();await page.getByRole('tooltip',{name:/Working folder.*project/s}).waitFor();
      const actions=panel.getByRole('button',{name:'Working folder actions'});
      const copy=page.getByRole('menuitem',{name:'Copy folder path',exact:true});
      const reveal=page.getByRole('menuitem',{name:'Show in folder',exact:true});
      await folder.press('Shift+F10');await copy.waitFor();
      assert(await reveal.isDisabled());
      assert(await copy.evaluate(node=>node===document.activeElement));
      await page.keyboard.press('Escape');await copy.waitFor({state:'detached'});
      assert(await folder.evaluate(node=>node===document.activeElement));
      await folder.press('ContextMenu');await copy.waitFor();await copy.press('Enter');
      await copy.waitFor({state:'detached'});
      assert.deepEqual(await page.evaluate(()=>window.copyResult()),{writes:['/home/work/project'],notice:'Copied'});
      assert(await panel.isVisible());
      await folder.click({button:'right'});await copy.waitFor();await copy.click();await copy.waitFor({state:'detached'});
      assert(await panel.isVisible());
      await actions.click();await copy.waitFor();await page.evaluate(()=>{window.rejectCopy=true;});
      const anchor=await page.locator('span[style*="--dsh-menu-trigger-"]').boundingBox(), opener=await actions.boundingBox();
      assert(Math.abs(anchor.x-opener.x)<=1 && Math.abs(anchor.y-opener.y-opener.height)<=1,JSON.stringify({anchor,opener}));
      const boundsAfterClick=await page.getByRole('menu').boundingBox();
      assert(Math.abs(boundsAfterClick.y-opener.y-opener.height)<=5,JSON.stringify({boundsAfterClick,opener}));
      await copy.click();await copy.waitFor({state:'detached'});
      assert.deepEqual(await page.evaluate(()=>window.copyResult()),{writes:['/home/work/project','/home/work/project'],notice:'Clipboard permission denied'});
      assert(await panel.isVisible());assert(await actions.evaluate(node=>node===document.activeElement));
      await page.keyboard.press('Escape');await panel.waitFor({state:'detached'});
      assert(await trigger.evaluate(node=>node===document.activeElement));
      await trigger.click();await page.getByRole('button',{name:'Outside'}).click();await panel.waitFor({state:'detached'});
      assert.equal(await page.evaluate(()=>localStorage.length),0);
      await page.setViewportSize({width:1576,height:880});
      await page.locator('[data-presentation="docked"]').waitFor();
      const bounds=await panel.boundingBox();assert(bounds.width>=300 && bounds.x+bounds.width<=1576);
      await trigger.click();await panel.waitFor({state:'detached'});
      await page.reload();await trigger.waitFor();assert.equal(await trigger.getAttribute('aria-expanded'),'false');
      await trigger.click();await panel.waitFor();assert.equal(await page.evaluate(()=>localStorage.length),0);
      await page.setViewportSize({width:390,height:844});await panel.waitFor({state:'detached'});
      const path='C:\\work\\project';await page.evaluate(path=>window.show(path),path);
      await trigger.press('Space');await panel.getByLabel('Working folder: '+path,{exact:true}).waitFor();
      assert.deepEqual(await panel.locator('[tabindex="0"] span').allTextContents(),['project','C:\\work']);
      await page.evaluate(()=>{window.rejectCopy=false;});
      await actions.click();const box=await copy.boundingBox();assert(box && box.x>=0 && box.x+box.width<=390);
      await copy.click();await copy.waitFor({state:'detached'});assert(await panel.isVisible());
      assert.equal((await page.evaluate(()=>window.copyResult())).writes.at(-1),path);
      await page.evaluate(path=>window.show(path,false),path);await panel.waitFor({state:'detached'});
      assert(await trigger.isDisabled());await page.evaluate(()=>window.show('/'));
      await trigger.click();await panel.getByText('/',{exact:true}).waitFor();
      await page.evaluate(()=>window.show('D:\\'));await panel.waitFor({state:'detached'});
      await trigger.click();await panel.getByText('D:\\',{exact:true}).waitFor();
      await page.evaluate(()=>{window.ownerChanged();window.folderRequests=[];});
      await actions.click();await reveal.click();await reveal.waitFor({state:'detached'});
      assert.deepEqual(await page.evaluate(()=>window.folderRequests),[{action:'desktop'},{action:'reveal',path:'D:\\'}]);
      await actions.click();await reveal.click();assert.equal(await page.evaluate(()=>window.folderRequests.length),3);
      await trigger.click();await panel.waitFor({state:'detached'});await trigger.click();await actions.click();
      await reveal.click();assert.equal(await page.evaluate(()=>window.folderRequests.length),4);
      await page.evaluate(()=>{window.folderRequests=[];window.show('/srv/ssh',true,false);});
      await trigger.click();await actions.click();assert(await reveal.isDisabled());assert(await copy.isEnabled());
      assert.deepEqual(await page.evaluate(()=>window.folderRequests),[]);
    });
  }
});
