import {detailMode} from './controls.mjs';
import assert from 'node:assert/strict';
import {join} from 'node:path';
import {writeFile} from 'node:fs/promises';

// Actual document, Rust Worker and Profile Settings; no synthetic view publication.
export async function verifyPresentation(page, pane, service, report, name) {
  const evidence=[];
  const peer=await page.context().newPage();
  await peer.goto(service.origin);
  await peer.locator('#reconnect').click();
  await peer.locator('#workbench').waitFor({state:'visible'});
  async function save(theme,size) {
    await page.getByRole('button',{name:'Appearance',exact:true}).click();
    await page.getByRole('combobox',{name:'Settings / appearance / theme',exact:true}).selectOption({label:theme});
    await page.getByRole('spinbutton',{name:'Settings / appearance / content_font_size',exact:true}).fill(String(size));
    const old=await page.getByRole('button',{name:'Save settings',exact:true}).elementHandle();
    await old.click();await page.waitForFunction(old=>!old.isConnected,old);await old.dispose();
    await page.getByRole('button',{name:'Close details',exact:true}).click();
    await page.waitForFunction(([theme,size])=>document.documentElement.dataset.theme===theme&&document.documentElement.dataset.contentSize===String(size),[theme,size]);
    await peer.waitForFunction(([theme,size])=>document.documentElement.dataset.theme===theme&&document.documentElement.dataset.contentSize===String(size),[theme,size]);
  }
  async function inspect(label) {
    const style=await pane.evaluate(pane=>{
      const root=getComputedStyle(document.documentElement), text=getComputedStyle(pane.querySelector('.composer textarea')),hint=getComputedStyle(pane.querySelector('.composer-hint'));
      const rgb=color=>color.match(/[\d.]+/g).slice(0,3).map(Number);
      const lum=color=>rgb(color).map(n=>{n/=255;return n<=.04045?n/12.92:((n+.055)/1.055)**2.4}).reduce((sum,n,i)=>sum+n*[.2126,.7152,.0722][i],0);
      const contrast=(one,two)=>{const a=lum(one),b=lum(two);return (Math.max(a,b)+.05)/(Math.min(a,b)+.05)};
      const surface=getComputedStyle(pane.querySelector('.composer')).backgroundColor;
      return {theme:document.documentElement.dataset.theme,scheme:root.colorScheme,font:text.fontSize,surface,text:text.color,contrast:contrast(text.color,surface),secondaryContrast:contrast(hint.color,surface),overflow:document.documentElement.scrollWidth>innerWidth};
    });
    assert(style.contrast>=4.5,JSON.stringify(style));assert(style.secondaryContrast>=4.5,JSON.stringify(style));assert(!style.overflow);
    evidence.push({label,...style});await page.screenshot({path:join(report,`${name}-${label}.png`)});
  }
  try {
    await save('dark',17);assert.equal(await pane.locator('.composer textarea').evaluate(node=>getComputedStyle(node).fontSize),'17px');await inspect('theme-dark');
    await save('light',12);await inspect('theme-light');
    await save('system',14);await page.emulateMedia({colorScheme:'dark'});await inspect('theme-system-dark');
    await page.emulateMedia({colorScheme:'light'});
    await page.getByRole('button',{name:'Toggle navigation',exact:true}).click();
    assert.equal(await page.locator('.workbench>.sidebar').count(),0);
    await page.getByRole('button',{name:'Toggle navigation',exact:true}).click();
    const separator=page.getByRole('separator',{name:'Resize navigation'});
    await separator.focus();const before=Number(await separator.getAttribute('aria-valuenow'));await separator.press('ArrowRight');
    assert.equal(Number(await separator.getAttribute('aria-valuenow')),before+10);await separator.press('ArrowLeft');
    await page.getByRole('button',{name:'Commands',exact:true}).click();
    await page.getByRole('combobox',{name:'Search commands'}).fill('appearance');
    await page.getByRole('combobox',{name:'Search commands'}).press('Enter');
    await page.getByRole('combobox',{name:'Settings / appearance / theme',exact:true}).waitFor();
    await page.getByRole('button',{name:'Close details',exact:true}).click();
    await page.locator('.conversation-menu').evaluate(node=>node.open=true);await page.getByLabel('Detail level').selectOption('standard');await page.locator('.conversation-menu').evaluate(node=>node.open=false);
    await pane.locator('.turn-summary').last().click();
    const summary=pane.locator('.message.tool .summary-toggle').last();
    await summary.scrollIntoViewIfNeeded();const summaryTop=await summary.evaluate(node=>node.getBoundingClientRect().top);
    await summary.click();assert.equal(await summary.getAttribute('aria-expanded'),'true');
    assert(Math.abs(await summary.evaluate(node=>node.getBoundingClientRect().top)-summaryTop)<2,'expansion preserves summary position');
    await page.screenshot({path:join(report,`${name}-chat-summary.png`)});
    await detailMode(page,'verbose');
    await page.setViewportSize({width:390,height:844});
    await page.getByRole('button',{name:'Toggle navigation',exact:true}).click();
    await page.getByRole('dialog',{name:'Workspace navigation',exact:true}).waitFor();
    await page.screenshot({path:join(report,`${name}-navigation-drawer.png`)});
    await page.keyboard.press('Escape');
    assert.equal(await page.getByRole('button',{name:'Toggle navigation',exact:true}).evaluate(node=>document.activeElement===node),true,'drawer restores trigger focus');
    await page.setViewportSize({width:1440,height:980});
    await waitForSavedPresentation(page);
    evidence.push({profileRefresh:true,keyboardResize:true,drawerFocus:true,summaryAnchor:true});
    await writeFile(join(report,`${name}-presentation.json`),JSON.stringify(evidence,null,2));
  } finally {await peer.close()}
}

export async function waitForSavedPresentation(page, timeout = 5000) {
  const deadline = Date.now() + timeout;
  do {
    const saved = await page.evaluate(async()=>{
      const opened=await new Promise((resolve,reject)=>{const request=indexedDB.open('rsi.presentation');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
      try {
        const records=await new Promise((resolve,reject)=>{const request=opened.transaction('layouts').objectStore('layouts').getAll();request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error)});
        return records.length>0;
      } finally { opened.close(); }
    });
    if (saved) return true;
    await new Promise(resolve=>setTimeout(resolve,25));
  } while (Date.now() < deadline);
  throw new Error('Timed out waiting for a saved layout');
}
