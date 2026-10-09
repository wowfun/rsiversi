// Typed product operations only. ElementHandles remain private to this helper.
import {randomBytes} from 'node:crypto';
const identity=()=>randomBytes(16).toString('hex');
const bounded=(s,n)=>Buffer.byteLength(s)<=n;
export async function sessionHelper(browser,policy,violation=()=>{}) {
  const context=browser.contexts()[0];
  if(context.pages().length!==1)throw new Error('one Session page required');
  const page=context.pages()[0];await page.setViewportSize({width:1280,height:720});
  let version=1,observation=null,nodes=new Map(),active=false,escaped=false,initialized=page.url()!=='about:blank';
  const valid=url=>{
    try{const u=new URL(url);return !u.username&&!u.password&&bounded(url,8192)&&(policy.mode==='public_web'?u.protocol==='https:'&&(!u.port||u.port==='443'):u.origin===policy.origin);}catch{return false;}
  };
  const invalidate=()=>{observation=null;for(const {handle}of nodes.values())void handle.dispose().catch(()=>{});nodes=new Map();};
  page.on('framenavigated',frame=>{if(frame===page.mainFrame()){initialized=true;version++;invalidate();}});
  context.on('page',popup=>{escaped=true;void popup.close();violation();});
  page.on('download',download=>{void download.cancel();});
  await context.route('**/*',async route=>{
    const request=route.request(),u=new URL(request.url());
    const main=request.isNavigationRequest()&&request.frame()===page.mainFrame();
    const allowed=main?valid(u.href):(!u.username&&!u.password&&(u.protocol==='https:'&&(!u.port||u.port==='443')||(policy.mode==='local_dev'&&u.origin===policy.origin)));
    if(allowed)await route.continue();else await route.abort('blockedbyclient');
  });
  const fingerprint=handle=>handle.evaluate(el=>{
    const tag=el.tagName.toLowerCase(),type=el.type??'',role=el.getAttribute('role')??({button:'button',a:'link',input:type==='checkbox'?'checkbox':type==='radio'?'radio':'textbox',textarea:'textbox',select:'combobox'}[tag]??tag);
    const name=el.getAttribute('aria-label')??el.labels?.[0]?.innerText??el.getAttribute('title')??el.innerText??'',href=el.getAttribute('href')??'';
    if(role.length>128||name.length>256||type.length>128||href.length>8192)return null;
    // Normalize before the private CDP transport crosses Rust's JSON boundary.
    // The escaped signature retains exact original UTF-16 semantics for freshness.
    return {role:role.toWellFormed(),name:name.toWellFormed(),type:type.toWellFormed(),href:href.toWellFormed(),signature:JSON.stringify([role,name,type,href]),disabled:!!el.disabled||el.getAttribute('aria-disabled')==='true',read_only:!!el.readOnly};
  });
  const snapshot=async()=>{
    invalidate();observation=identity();
    const text=await page.evaluate(()=>{let out='';const walker=document.createTreeWalker(document.body??document.documentElement,NodeFilter.SHOW_TEXT);let current,count=0;while((current=walker.nextNode())&&out.length<8192&&count++<8192){if(!['SCRIPT','STYLE','NOSCRIPT'].includes(current.parentElement?.tagName))out+=current.textContent.slice(0,8192-out.length)+'\n';}return out.toWellFormed();});
    const candidates=page.locator('a,button,input:not([type=file]),textarea,select,[role=button],[role=link],[role=textbox]');
    const total=await candidates.count(),count=Math.min(total,256);const structure=[];let truncated=total>count;
    const initial=Buffer.byteLength(JSON.stringify({text,nodes:[]}));
    let retained=initial;const charges=[];
    for(let index=0;index<count;index++){
      const handle=await candidates.nth(index).elementHandle();if(!handle)continue;
      if(!await handle.isVisible()){await handle.dispose();continue;}
      const state=await fingerprint(handle);if(!state){await handle.dispose();truncated=true;continue;}
      const {signature,...publicState}=state;
      const id=String(index+1);const item={id,...publicState,href:state.href.slice(0,512).toWellFormed()};
      const bytes=Buffer.byteLength(JSON.stringify(item))+(structure.length?1:0);
      if(retained+bytes>60*1024){await handle.dispose();truncated=true;break;}
      retained+=bytes;charges.push(bytes);
      nodes.set(id,{handle,state});structure.push(item);
    }
    const result={url:page.url(),document_version:String(version),observation_id:observation,text,nodes:structure,truncated};
    let complete=Buffer.byteLength(JSON.stringify({...result,nodes:[]}))+retained-initial;
    while(complete>64*1024&&structure.length){
      const item=structure.pop();complete-=charges.pop();
      const entry=nodes.get(item.id);nodes.delete(item.id);await entry.handle.dispose();
      result.truncated=true;
    }
    if(complete>64*1024)throw new Error('observation envelope exceeds bound');
    return result;
  };
  return async packet=>{
    if(active)return {status:'not_started',reason:'busy',retry_after_ms:1000};
    active=true;
    let timer;
    const work=async()=>{
      if(escaped)throw new Error('single-page invariant failed');
      const op=packet.operation;
      if(!valid(page.url())&&(initialized||page.url()!=='about:blank'||op!=='navigate'))throw new Error('page escaped frozen navigation policy');
      if(op==='navigate'){
        if(!valid(packet.url))return {status:'not_started',reason:'policy_blocked'};
        initialized=true;invalidate();await page.goto(packet.url,{waitUntil:'domcontentloaded',timeout:20000});
        if(!valid(page.url()))throw new Error('navigation escaped frozen policy');
        return {status:'completed',snapshot:await snapshot()};
      }
      if(op==='observe')return {status:'completed',snapshot:await snapshot()};
      if(op==='screenshot'){
        const png=await page.screenshot({type:'png',fullPage:false,timeout:10000});
        if(png.length>4*1024*1024)return {status:'screenshot_unavailable'};
        return {status:'completed',png:png.toString('base64')};
      }
      if(op==='click'||op==='fill'){
        if(packet.document_version!==String(version)||packet.observation_id!==observation)return {status:'not_started',reason:'stale_observation'};
        const target=nodes.get(packet.node);if(!target)return {status:'not_started',reason:'unknown_node'};
        const state=await fingerprint(target.handle).catch(()=>null);
        const actionable=state&&JSON.stringify(state)===JSON.stringify(target.state)&&!state.disabled&&(op!=='fill'||!state.read_only)&&await target.handle.isVisible()&&await target.handle.evaluate(el=>{if(!el.isConnected)return false;const r=el.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return !!hit&&(el===hit||el.contains(hit));});
        if(!actionable)return {status:'not_started',reason:'node_changed_or_not_actionable'};
        if(op==='fill'&&(typeof packet.text!=='string'||!['input','textarea'].includes(await target.handle.evaluate(el=>el.tagName.toLowerCase()))||!bounded(packet.text,16384)))return {status:'not_started',reason:'invalid_fill'};
        // From dispatch onward a failure is uncertain; the Rust owner retires.
        if(op==='click')await target.handle.click({timeout:5000});else await target.handle.fill(packet.text,{timeout:5000});
        invalidate();if(!valid(page.url()))throw new Error('action escaped frozen policy');
        return {status:'completed',snapshot:await snapshot()};
      }
      if(op==='scroll'){
        if(!['up','down'].includes(packet.direction))return {status:'not_started',reason:'invalid_scroll'};
        await page.evaluate(direction=>window.scrollBy(0,(direction==='down'?1:-1)*window.innerHeight),packet.direction);invalidate();
        return {status:'completed',snapshot:await snapshot()};
      }
      return {status:'not_started',reason:'unknown_operation'};
    };
    const budget=packet.operation==='navigate'?20000:['observe','screenshot'].includes(packet.operation)?10000:5000;
    try{
      const result=await Promise.race([work(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('Session helper operation deadline elapsed')),budget);})]);
      if(['completed','screenshot_unavailable'].includes(result.status)){
        if(escaped||!valid(page.url()))throw new Error('page escaped frozen navigation policy');
        result.url=page.url();
        if(result.snapshot&&result.snapshot.url!==result.url)throw new Error('document changed during observation');
      }
      return result;
    }
    finally{clearTimeout(timer);active=false;}
  };
}
