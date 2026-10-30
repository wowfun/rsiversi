import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';
import {sourceFixture} from './source-fixture.mjs';

// Actual React notice/activity owners and built CSS; no Worker or transport claim.
export async function verifyAccessibility(browser, repository, report, name) {
  const code=`
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {Notices} from './src/notices.tsx';
import {ActivityLabel} from './src/activity-label.tsx';
import {setNotice,setStorageNotice,report} from './src/bridge.ts';
import {ResizeHandle} from './src/presentation.tsx';
import './styles.css';import shell from './src/shell.module.css';import navigation from './src/navigation.module.css';import session from './src/session/surface.module.css';import chat from './src/session/chat.module.css';
function ResizeProbe({name,reverse=false}){const [width,setWidth]=useState(280);return <aside className={reverse?'resources':undefined} style={{position:'relative',width,height:40,border:0,borderInlineEnd:'1px solid var(--line)'}}>Resize {name}<ResizeHandle name={name} value={width} min={200} max={420} reverse={reverse} onChange={setWidth}/></aside>}
createRoot(document.getElementById('root')).render(<main id="workbench" className={[shell.scope,navigation.scope,session.scope,chat.scope].join(' ')} style={{padding:32,maxWidth:720,margin:'auto'}}>
<h1>Conversation accessibility</h1><Notices/>
<nav aria-label="Conversations">{['running','waiting','unread','unknown',undefined].map(state=><div className="session-row" key={state??'unobserved'}>
<button className="nav-item"><strong>Conversation {state??'unobserved'}</strong><ActivityLabel state={state}/></button></div>)}</nav>
<form className="composer"><label htmlFor="message">Message</label><textarea id="message"/></form>
<ResizeProbe name="navigation"/><ResizeProbe name="resources" reverse/>
<button onClick={()=>setStorageNotice('Draft storage is unavailable')}>Storage warning</button>
<button onClick={()=>report(new Error('Request failed'))}>Report error</button>
<button onClick={()=>setNotice('')}>Clear ordinary notice</button>
<button onClick={()=>setStorageNotice('')}>Disconnect</button></main>);
`;
  const server=await sourceFixture({name:'accessibility-fixture',source:code,root:join(repository,'apps/web')});
  let page;const errors=[],evidence=[];
  try {
    page=await browser.newPage({viewport:{width:1100,height:850}});
    page.on('pageerror',error=>errors.push(error.message));
    await page.goto(server.url);
    await page.getByRole('button',{name:'Storage warning',exact:true}).click();
    await page.getByRole('button',{name:'Report error',exact:true}).click();
    assert.deepEqual(await page.locator('#notice p').allTextContents(),['Request failed','Draft storage is unavailable']);
    const notice=await page.locator('#notice').elementHandle();
    await page.getByRole('button',{name:'Report error',exact:true}).click();
    assert(await notice.evaluate(node=>node===document.getElementById('notice')));
    for(const [theme,forcedColors] of [['light','none'],['dark','none'],['light','active']]) {
      await page.emulateMedia({colorScheme:theme,forcedColors});
      await page.getByLabel('Message',{exact:true}).focus();
      const focus=await page.getByLabel('Message',{exact:true}).evaluate(node=>({focused:document.activeElement===node,style:getComputedStyle(node).outlineStyle,width:parseFloat(getComputedStyle(node).outlineWidth),border:parseFloat(getComputedStyle(node).borderWidth),caret:getComputedStyle(node).caretColor}));
      assert(focus.focused,JSON.stringify(focus));
      if(forcedColors==='active')assert(focus.style==='solid'&&focus.width>=2,JSON.stringify(focus));
      else assert(focus.style==='none'&&focus.border===0,JSON.stringify(focus));
      for(const side of ['navigation','resources']){
        const handle=page.getByRole('separator',{name:'Resize '+side,exact:true});
        await page.getByRole('heading',{name:'Conversation accessibility'}).click();await handle.hover();
        const transparent=()=>handle.evaluate(node=>getComputedStyle(node).backgroundColor.endsWith(', 0)'));
        assert(await transparent(),side+' hover must not paint the hit area');
        const before=Number(await handle.getAttribute('aria-valuenow')),rect=await handle.boundingBox(),direction=side==='navigation'?1:-1;
        await page.mouse.move(rect.x+rect.width/2,rect.y+rect.height/2);await page.mouse.down();await page.mouse.move(rect.x+rect.width/2+20*direction,rect.y+rect.height/2);
        assert(await transparent(),side+' drag must not paint the hit area');await page.mouse.up();
        assert.equal(Number(await handle.getAttribute('aria-valuenow')),before+20);
        await page.getByLabel('Message',{exact:true}).focus();await page.keyboard.press('Tab');if(side==='resources')await page.keyboard.press('Tab');await page.keyboard.press('ArrowRight');
        assert.equal(Number(await handle.getAttribute('aria-valuenow')),before+20+10*direction);
        const indicator=await handle.evaluate(node=>({focused:node.matches(':focus-visible'),outline:getComputedStyle(node).outlineStyle,width:getComputedStyle(node,'::after').width,background:getComputedStyle(node,'::after').backgroundColor}));
        assert(indicator.focused&&indicator.outline==='none'&&indicator.width==='2px'&&!indicator.background.endsWith(', 0)'),JSON.stringify(indicator));
      }
      await page.getByLabel('Message',{exact:true}).focus();
      for(const label of ['Running','Waiting for you','New activity','Unknown']) assert.equal(await page.locator('.session-activity').filter({hasText:label}).count(),1);
      assert.equal(await page.locator('.session-state[aria-hidden="true"]').count(),4);
      const unobserved=page.getByRole('button',{name:'Conversation unobserved',exact:true});
      assert.equal(await unobserved.locator('.session-activity').count(),0);
      const rowHeights=await page.locator('nav .session-row').evaluateAll(rows=>rows.map(row=>row.getBoundingClientRect().height));
      assert(rowHeights.every(height=>height===rowHeights[0]&&height<=36),`Activity dots must not add a secondary row: ${rowHeights}`);
      const names=await page.locator('nav').ariaSnapshot();
      for(const label of ['Running','Waiting for you','New activity','Unknown']) assert(names.includes(label),names);
      assert(await page.locator('#notice p').evaluateAll(nodes=>nodes[0].getBoundingClientRect().bottom<=nodes[1].getBoundingClientRect().top));
      await page.screenshot({path:join(report,`${name}-accessibility-${theme}-${forcedColors}.png`)});
      evidence.push({theme,forcedColors,focus,activityLabels:true,rowHeights,noticesSeparate:true});
    }
    await page.getByRole('button',{name:'Clear ordinary notice',exact:true}).click();
    assert.deepEqual(await page.locator('#notice p').allTextContents(),['Draft storage is unavailable']);
    await page.getByRole('button',{name:'Disconnect',exact:true}).click();
    assert.equal(await page.locator('#notice').isVisible(),false);
    assert.deepEqual(errors,[]);
    await writeFile(join(report,`${name}-accessibility.json`),JSON.stringify({evidence,errors},null,2));
  } finally {await page?.close();await server.close();}
}
