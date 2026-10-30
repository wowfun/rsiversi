import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';

export async function verifyVisibilityDom(page, report, name) {
  await page.evaluate(async () => {
    const pane=fixture.runtime.panes.get('main');
    window.visibilityProof={pane,send:pane.sendVisible,surface:structuredClone(fixture.view.surfaces.main),reads:0,publications:[]};
    pane.sendVisible=async()=>{visibilityProof.publications.push(pane.visiblePending.keys);pane.visiblePending=undefined;};
    pane.transcript.style.cssText='height:220px;max-height:220px;flex:none;overflow:auto';
    Object.assign(fixture.view.surfaces.main,{generation:'visibility-proof',ui_cards:true,transcript:{status:'Ready',omitted:false,blocks:Array.from({length:128},(_,i)=>({key:String(i),role:'assistant',title:'Assistant',text:'Visible content '+i,sources:0}))}});await fixture.frame();
    pane.visibleKey=undefined;
    pane.syncVisible(); // A render/resize frame can arrive before IntersectionObserver's first delivery.
    visibilityProof.beforeObservation=structuredClone(visibilityProof.publications);
    for(const entry of pane.blocks.values()) {
      const read=entry.node.getBoundingClientRect.bind(entry.node);
      entry.node.getBoundingClientRect=()=>{window.visibilityProof.reads++;return read();};
    }
  });
  try {
    assert.deepEqual(await page.evaluate(()=>visibilityProof.beforeObservation),[],'unobserved blocks must not publish an empty selection');
    await page.waitForFunction(()=>visibilityProof.pane.visibleBlocks.last(4).some(([key])=>key==='127'));
    await page.waitForFunction(()=>visibilityProof.publications.length>0);
    const initial=await page.evaluate(()=>visibilityProof.publications);
    assert(initial.every(keys=>keys.length>0),JSON.stringify(initial));
    await page.evaluate(()=>{visibilityProof.reads=0;visibilityProof.pane.transcript.scrollTop=0;});
    await page.waitForFunction(()=>visibilityProof.pane.visibleBlocks.last(4).some(([key])=>key==='0'));
    const evidence=await page.evaluate(()=>{
      const {pane,reads}=visibilityProof;
      const bounds=pane.transcript.getBoundingClientRect();
      const expected=[...pane.blocks].filter(([,entry])=>{const rect=Element.prototype.getBoundingClientRect.call(entry.node);return rect.height>0&&rect.bottom>bounds.top&&rect.top<bounds.bottom;}).slice(-4).map(([key])=>key);
      return {blocks:pane.blocks.size,scrollGeometryReads:reads,expected,actual:pane.visibleBlocks.last(4).map(([key])=>key)};
    });
    assert.equal(evidence.blocks,128);assert.equal(evidence.scrollGeometryReads,0);assert.deepEqual(evidence.actual,evidence.expected);
    await page.screenshot({path:join(report,`${name}-visibility.png`)});
    const pending=await page.evaluate(async()=>{
      const {pane}=visibilityProof;
      fixture.view.surfaces.main.transcript={status:'Ready',omitted:false,blocks:[{key:'fresh',role:'assistant',title:'Assistant',text:'Not observed yet',sources:0}]};await fixture.frame();
      const pending=!pane.visibleBlocks.ready;
      pane.uiCards=false;pane.visibleKey=undefined;pane.syncVisible();
      return pending;
    });
    assert(pending,'the disabling check must occur before initial observation');
    assert.deepEqual(await page.evaluate(()=>visibilityProof.publications.at(-1)),[],'disabling inline cards clears immediately');
    evidence.initialPublications=initial;
    await writeFile(join(report,`${name}-visibility.json`),JSON.stringify(evidence,null,2));
  } finally {
    await page.evaluate(async()=>{
      const {pane,send,surface}=visibilityProof;
      pane.transcript.style.cssText='';pane.sendVisible=send;
      fixture.view.surfaces.main=surface;await fixture.frame();delete window.visibilityProof;
    });
  }
}
