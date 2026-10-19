import assert from 'node:assert/strict';
import {join} from 'node:path';
import {documentModules} from './document-modules.mjs';

// Real document controls and shipped modules, without claiming ACP transport coverage.
export async function verifyExternalDraftDom(browser, root, report, name) {
  const page = await browser.newPage({viewport:{width:1440,height:980}});
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  try {
    await documentModules(page,root,'http://rsi-external.invalid','<!doctype html><html><head><link rel="stylesheet" href="/styles.css"></head><body><main id="panes" class="panes"></main></body></html>',['external-pane.js','external-drafts.js','composer-actions.js','styles.css']);
    await page.evaluate(async()=>{
      const {externalPaneClass}=await import('/external-pane.js');
      const element=(tag,className,text)=>{const node=document.createElement(tag);node.className=className;if(text)node.textContent=text;return node;};
      const perform=work=>Promise.resolve().then(work).catch(()=>{});
      const button=(label,action,className)=>{const node=element('button',className,label);node.type='button';node.addEventListener('click',()=>perform(action));return node;};
      let calls=0,fail=true;
      const Pane=externalPaneClass({element,button,perform,command:async()=>{calls++;if(fail)throw Error('unknown');}});
      const frame=(id,generation=id)=>({generation,external:{observed:{connected:true,snapshot:{id,generation:'1',endpoint:'Fixture agent',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:[],following:true,busy:false}});
      const panes=[new Pane(0),new Pane(1)];panes.forEach(pane=>pane.render(frame('conversation-A')));panes[0].node.classList.add('selected');
      window.externalFixture={Pane,panes,frame,calls:()=>calls,succeed:()=>{fail=false;}};
    });
    const panes=page.locator('.external-pane'), input=panes.first().locator('textarea');
    await input.fill('Retain this prompt · 保留原始草稿');
    assert.equal(await panes.nth(1).locator('textarea').inputValue(),await input.inputValue());
    await panes.first().getByTestId('composer-send').click();
    await panes.first().getByRole('alert').filter({hasText:'outcome is unknown'}).waitFor();
    await page.evaluate(()=>{const {panes,frame}=window.externalFixture;panes[0].render(frame('conversation-B'));panes[0].render(frame('conversation-A','reattached'));});
    await input.press('ArrowLeft');
    assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),true);
    assert.equal(await panes.nth(1).getByTestId('composer-send').isDisabled(),true);
    assert.equal(await page.evaluate(()=>window.externalFixture.calls()),1);
    assert.equal(await input.inputValue(),'Retain this prompt · 保留原始草稿');
    const mutations=await page.evaluate(async()=>{
      const pane=window.externalFixture.panes[0];
      const changes=[];
      const observer=new MutationObserver(records=>changes.push(...records));
      observer.observe(pane.notice,{subtree:true,childList:true,characterData:true,attributes:true});
      for(let i=0;i<3;i++){
        pane.renderComposer();
        pane.input.dispatchEvent(new Event('input',{bubbles:true}));
      }
      await Promise.resolve();observer.disconnect();return changes.length;
    });
    assert.equal(mutations,0,'unchanged warning must not retrigger the live region');
    await page.screenshot({path:join(report,`${name}-external-unknown.png`),fullPage:true});
    await page.setViewportSize({width:420,height:860});
    await page.screenshot({path:join(report,`${name}-external-unknown-narrow.png`),fullPage:true});
    await input.fill('An explicitly revised prompt');
    assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),false);
    assert.equal(await panes.nth(1).getByTestId('composer-send').isDisabled(),false);
    await page.evaluate(()=>{
      const {Pane,panes,frame}=window.externalFixture;
      for(let i=0;i<15;i++){const pane=new Pane(i+2);pane.render(frame(`pinned-${i}`));pane.node.hidden=true;panes.push(pane);}
      const blocked=panes[1];blocked.render(frame('overflow'));
      const state=frame('overflow');state.external.blocks=[{key:'history',role:'agent',text:'Retained observation while draft capacity is full',source:{}}];
      blocked.render(state);window.externalFixture.blockedState=state;window.externalFixture.history=blocked.transcript.firstChild;
      blocked.render(state);blocked.render(state);
    });
    assert.equal(await panes.nth(1).locator('textarea').isDisabled(),true);
    assert.equal(await page.evaluate(()=>window.externalFixture.history===window.externalFixture.panes[1].transcript.firstChild),true);
    await page.setViewportSize({width:1440,height:980});
    await page.screenshot({path:join(report,`${name}-external-capacity.png`),fullPage:true});
    await page.evaluate(()=>{const {panes}=window.externalFixture;panes[0].node.classList.remove('selected');panes[1].node.classList.add('selected');});
    await page.setViewportSize({width:420,height:860});
    await page.screenshot({path:join(report,`${name}-external-capacity-narrow.png`),fullPage:true});
    await page.evaluate(()=>{const {panes,blockedState}=window.externalFixture;panes[2].reset();panes[1].render(blockedState);});
    assert.equal(await panes.nth(1).locator('textarea').isDisabled(),false);
    await panes.nth(1).locator('textarea').fill('Capacity released · 可继续编辑');
    assert.equal(await panes.nth(1).getByTestId('composer-send').isDisabled(),false);
    await page.evaluate(()=>{
      const fixture=window.externalFixture,{Pane,panes,frame}=fixture;
      panes.forEach(pane=>pane.reset());document.querySelector('#panes').replaceChildren();panes.length=0;
      fixture.succeed();fixture.admitted='Admitted draft\n'+'x'.repeat(65500);
      fixture.admitted=fixture.admitted.padEnd(65536,'x');
      for(let i=0;i<16;i++){
        const pane=new Pane(i);pane.render(frame(`full-${i}`));pane.input.value=fixture.admitted;
        assertSaved(pane);pane.node.hidden=i>0;panes.push(pane);
      }
      panes[0].node.classList.add('selected');
      function assertSaved(pane){if(!pane.save())throw Error('fixture draft admission failed');}
    });
    const admitted=await page.evaluate(()=>window.externalFixture.admitted);
    await input.press('Control+End');await input.press('!');
    assert.equal(await input.inputValue(),admitted);
    const rejected=await page.evaluate(()=>{const p=window.externalFixture.panes[0];return {capacity:p.capacity,notice:p.notice.textContent,bytes:p.record.bytes};});
    assert.equal(rejected.capacity,true,JSON.stringify(rejected));
    assert.match(rejected.notice,/capacity is full/);
    assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),false);
    await page.setViewportSize({width:1440,height:980});
    await page.screenshot({path:join(report,`${name}-external-rejected-edit.png`),fullPage:true});
    await page.setViewportSize({width:420,height:860});
    await page.screenshot({path:join(report,`${name}-external-rejected-edit-narrow.png`),fullPage:true});
    await page.evaluate(()=>{
      const {Pane,panes,frame}=window.externalFixture;
      const sibling=new Pane(16);sibling.render(frame('full-0'));panes.push(sibling);
    });
    await page.setViewportSize({width:1440,height:980});
    await panes.last().locator('textarea').fill('A shorter accepted draft · 已恢复');
    assert.equal(await input.inputValue(),'A shorter accepted draft · 已恢复');
    assert.equal(await panes.first().getByRole('alert').isVisible(),false);
    for(const theme of ['light','dark']) {
      await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
      await page.screenshot({path:join(report,`${name}-external-sibling-recovery-${theme}.png`),fullPage:true});
    }
    await input.fill(admitted);await input.press('Control+End');await input.press('!');
    assert.equal(await input.inputValue(),admitted);
    await page.setViewportSize({width:420,height:860});
    await panes.first().getByTestId('composer-send').click();
    await page.waitForFunction(()=>window.externalFixture.panes[0].input.value==='');
    assert.equal(await page.evaluate(()=>window.externalFixture.calls()),2);
    await input.fill(admitted);await input.press('Control+End');await input.press('!');
    assert.equal(await input.inputValue(),admitted);
    assert.equal(await panes.first().getByTestId('composer-send').isDisabled(),false);
    await input.press('Enter');
    await page.waitForFunction(()=>window.externalFixture.panes[0].input.value==='');
    assert.equal(await page.evaluate(()=>window.externalFixture.calls()),3);
    assert.deepEqual(errors,[]);
  } finally {await page.close();}
}
