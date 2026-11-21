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
    document.body.style.margin='0';
    const root=createRoot(document.querySelector('#root'));
    window.show=(path='/home/work/project',chat=true)=>root.render(<><IconSprite/>
      <header className="pane-header"><div className="pane-heading"><SessionPanel key={path} path={path} chat={chat}/></div></header>
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
      await page.evaluate(path=>window.show(path,false),path);await panel.waitFor({state:'detached'});
      assert(await trigger.isDisabled());await page.evaluate(()=>window.show('/'));
      await trigger.click();await panel.getByText('/',{exact:true}).waitFor();
      await page.evaluate(()=>window.show('D:\\'));await panel.waitFor({state:'detached'});
      await trigger.click();await panel.getByText('D:\\',{exact:true}).waitFor();
    });
  }
});
