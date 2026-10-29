import assert from "node:assert/strict";
import { join } from "node:path";
import { workerConformance } from "./worker-conformance.mjs";

export async function verifyFrameDom(page, report, browser) {
  const result = await page.evaluate(async () => {
    const {runtime,view}=fixture,panes=runtime.panes;
    const present=(frame)=>runtime.presentFrame(frame,{});
    const block=(key,text)=>({key,text,role:'assistant',title:'Assistant',sources:0,clipped:false});
    const pane=index=>({...structuredClone(view.surfaces.main),generation:'patch-1',session:`frame-session-${index}`,transcript:{blocks:[block('a','First block'),block('b','Second block')],status:'Ready'}});
    const snapshot={...view,surfaces:{main:pane(0),compare:pane(1)}};
    await present({kind:'snapshot',frame_id:'100',view:snapshot});
    const first=panes.get('main').blocks.get('a').node,second=panes.get('main').blocks.get('b').node,right=panes.get('compare').blocks.get('a').node;
    await Promise.all([...panes.values()].map(pane=>pane.binding));
    runtime.select('compare');await new Promise(resolve=>requestAnimationFrame(resolve));panes.get('compare').input.focus();
    const patched=await present({kind:'patch',frame_id:'101',base_frame_id:'100',sections:{},surfaces:[{surface:'main',fields:{},transcript:{fields:{},upsert:[block('b','Updated second')],remove:[]}}]});
    const identities=first===panes.get('main').blocks.get('a').node&&second===panes.get('main').blocks.get('b').node&&right===panes.get('compare').blocks.get('a').node;
    const focus=document.activeElement===panes.get('compare').input;
    const stale=await present({kind:'patch',frame_id:'104',base_frame_id:'103',sections:{notice:'must not appear'},surfaces:[]});
    const retained=runtime.view.notice===''&&panes.get('main').blocks.get('b').text.textContent==='Updated second';
    const reordered=await present({kind:'patch',frame_id:'102',base_frame_id:'101',sections:{},surfaces:[{surface:'main',fields:{},transcript:{fields:{},upsert:[block('c','New block')],remove:['a'],order:['c','b']}}]});
    const order=[...panes.get('main').transcript.querySelectorAll('.message-text')].map(node=>node.textContent);
    const replacement=pane(0);replacement.generation='patch-2';
    await present({kind:'snapshot',frame_id:'105',view:{...snapshot,surfaces:{main:replacement,compare:pane(1)}}});
    const replaced=second!==panes.get('main').blocks.get('b').node;
    replacement.active='displayed-turn';
    await present({kind:'snapshot',frame_id:'106',view:{...snapshot,surfaces:{main:replacement,compare:pane(1)}}});
    const acknowledgedStop=structuredClone(panes.get('main').stopBinding);
    const connection=runtime.connection,originalRender=connection.render.bind(connection);let release,entered;
    const rendering=new Promise(resolve=>entered=resolve);
    connection.render=()=>new Promise(resolve=>{release=resolve;entered()});
    const applying=present({kind:'patch',frame_id:'107',base_frame_id:'106',sections:{},surfaces:[{surface:'main',fields:{active:'new-turn'}}]});
    await rendering;
    const stopFenced=panes.get('main').cancel.disabled&&!panes.get('main').stopBinding;
    release(undefined);await applying;connection.render=originalRender;
    const nextStop=panes.get('main').stopBinding.turn_id;
    return {patched:patched.accepted,identities,focus,stale,retained,reordered:reordered.accepted,order,replaced,acknowledgedStop,stopFenced,nextStop};
  });
  assert.deepEqual(result, { patched: true, identities: true, focus: true, stale: false, retained: true, reordered: true, order: ["New block", "Updated second"], replaced: true, acknowledgedStop:{generation:"patch-2",turn_id:"displayed-turn"},stopFenced:true,nextStop:"new-turn" });
  await page.screenshot({ path: join(report, `${browser}-incremental-frame-dom.png`) });
}

// Actual Worker/Rust application and authenticated HTTP; the document deliberately
// withholds acknowledgement. This does not infer cleanup from DOM disappearance.
export async function verifyAcknowledgementDeadline(page, service) {
  const {paired}=await import('./paired-env.mjs');
  const receipt = service.register("ack deadline fixture");
  const { family: buildFamily } = paired;
  const conformance = await page.evaluate(workerConformance, {receipt: service.register("Worker conformance"), buildFamily});
  assert.equal(conformance.ok, true);
  const result = await page.evaluate(async ({receipt, buildFamily}) => {
    const worker = new Worker("/worker.js", { type: "module" });
    let sequence = 0, frameCount = 0;
    const pending = new Map();
    let receiveFrame, fail;
    const first = new Promise(resolve => { receiveFrame = resolve; });
    const failed = new Promise(resolve => { fail = resolve; });
    const call = (method, payload) => new Promise((resolve, reject) => {
      const id = ++sequence; pending.set(id, { resolve, reject }); worker.postMessage({ kind: "call", id, method, payload });
    });
    worker.onmessage = ({ data }) => {
      if (data.kind === "view") { frameCount++; receiveFrame(JSON.parse(data.view)); }
      if (data.kind === "failed") fail(data.error);
      if (data.kind === "reply") { const waiter = pending.get(data.id); pending.delete(data.id); if (data.error) waiter?.reject(new Error(data.error)); else waiter?.resolve(data.result); }
    };
    let timer;
    try {
      const deadline = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("ACK expiry failed to drain Worker")), 40_000); });
      return await Promise.race([deadline, (async () => {
        await call("connect", { receipt: JSON.stringify(receipt), devHttp: false, buildFamily });
        const frame = await first;
        const workspace = frame.view.catalog.workspaces[0].id;
        await call("command", JSON.stringify({action:"add_surface",pane:"compare"}));
        for (const pane of ["main", "compare"]) await call("command", JSON.stringify({ action: "create", pane, workspace }));
        // A stale acknowledgement must not release the pending frame or renew its deadline.
        worker.postMessage({ kind: "ack", frame_id: "18446744073709551615" });
        const error = await failed;
        const resources = await call("resources");
        return { frameCount, error, resources };
      })()]);
    } finally { clearTimeout(timer); worker.terminate(); }
  }, {receipt, buildFamily});
  assert.equal(result.frameCount, 1);
  assert.match(result.error, /acknowledgement timed out/);
  assert.deepEqual(result.resources, { pending_timers: 0, active_alarms: 0, active_requests: 0 });
  return { ...result, conformance };
}
