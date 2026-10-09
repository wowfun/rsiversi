// Fixed helper asset. Its caller supplies a confined Process and bounded framed ports.
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { WebSocketServer } from 'ws';
import { chromium } from 'playwright';

import {NullFrameReader} from './frame-reader.mjs';
import {deadline} from './deadline.mjs';
import {PacketOutput, ProxySocket} from './proxy-flow.mjs';
import {createProxy} from './http-proxy.mjs';
import {sessionHelper} from './session-helper.mjs';
import {cdpPacket,decodeCdp} from './cdp.mjs';

const mode = process.argv[2];
const maximumFrame = 8 * 1024 * 1024;
const output = new PacketOutput(process.stdout, () => terminate(), () => {
  for (const socket of sockets.values()) socket.pump();
});
const emit = packet => output.emit(packet);
const sockets = new Map();
let child, browser, peer, proxy, control, session;
let initialized = false, dialogs = 0, privateSequence = -100000;
const privateResponses=new Map();
const terminate = () => {
  for (const socket of sockets.values()) socket.destroy();
  peer?.close(); control?.close(); proxy?.close();
  child?.kill('SIGTERM');
  if (child) setTimeout(() => child?.kill('SIGKILL'), 1000).unref();
};
process.on('SIGTERM', terminate);
process.stdin.on('end', terminate);
process.stdout.on('error', terminate);
process.on('uncaughtException', () => { terminate(); process.exitCode=1; input.close(); process.stdin.destroy(); });
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', async line => {
  try {
    if (Buffer.byteLength(line)>maximumFrame) throw new Error('input frame exceeds bound');
    const packet=JSON.parse(line);
    if (packet.kind==='init' && !initialized) {
      initialized=true;
      if (mode==='browser') await startBrowser(packet);
      else if (mode==='checker'||mode==='mcp'||mode==='session') await startClient(packet);
      else throw new Error('unknown helper mode');
    } else if (!initialized) throw new Error('helper is not initialized');
    else if (packet.kind==='cdp') {
      const value=JSON.stringify(decodeCdp(packet.value));
      if (mode==='browser') child.stdio[3].write(`${value}\0`);
      else if (peer?.readyState===1) peer.send(value);
    } else if (packet.kind==='proxy_opened') {
      const socket=sockets.get(packet.id);
      socket?.open();
    } else if (packet.kind==='proxy_data') {
      sockets.get(packet.id)?.receive(packet.data);
    } else if (packet.kind==='proxy_ack') {
      sockets.get(packet.id)?.acknowledge(packet.bytes);
    } else if (packet.kind==='proxy_close') {
      sockets.get(packet.id)?.close();
    } else if (packet.kind==='mcp' && mode==='mcp') child.stdin.write(`${JSON.stringify(packet.value)}\n`);
    else if (packet.kind==='check' && mode==='checker') await check(packet);
    else if (packet.kind==='session' && mode==='session') emit({kind:'session_result',result:await session(packet)});
    else if (packet.kind==='page_state' && mode==='browser') {
      const id=privateSequence--;privateResponses.set(id,'page_state');
      child.stdio[3].write(`${JSON.stringify({id,method:'Target.getTargets'})}\0`);
    }
    else if (packet.kind==='navigate' && mode==='checker') {
      const timeout=deadline(20000);
      const page=browser.contexts()[0].pages()[0];
      await page.goto(packet.url,{waitUntil:'domcontentloaded',timeout:timeout(20000)});
      emit({kind:'observation',snapshot:await observe(page,timeout),url:page.url()});
    } else if (packet.kind==='observe' && mode==='checker') {
      const page=browser.contexts()[0].pages()[0];
      emit({kind:'observation',snapshot:await observe(page),url:page.url()});
    } else if (packet.kind==='close') { terminate(); input.close(); process.stdin.destroy(); }
    else throw new Error('unknown helper packet');
  } catch (error) {
    emit({kind:'error',error:String(error.message).slice(0,1024)});
    terminate();
  }
});
async function startBrowser(packet) {
  let sequence=0;
  proxy=createProxy(({socket,host,port,transport,head,connect})=>{
    if (sockets.size>=8 || head.length>32768) { socket.destroy(); return; }
    const id=++sequence;
    const flow=new ProxySocket(socket,id,head,output,()=>{sockets.delete(id);emit({kind:'proxy_close',id});},connect);
    sockets.set(id,flow);
    emit({kind:'proxy_open',id,host,port,transport});
  },packet.session_policy?.mode==='local_dev'?new URL(packet.session_policy.origin):null);
  await new Promise(resolve=>proxy.listen(0,'127.0.0.1',resolve));
  child=spawn('/runtime/chrome/chrome',[
    '--headless','--disable-gpu','--no-first-run','--no-default-browser-check',
    '--disable-background-networking','--disable-component-update','--disable-dev-shm-usage',
    '--user-data-dir=/tmp/browser-data','--remote-debugging-pipe',
    `--proxy-server=http://127.0.0.1:${proxy.address().port}`,'--proxy-bypass-list=<-loopback>',
    ...(packet.fixture_tls ? ['--ignore-certificate-errors'] : []),
    'about:blank'
  ],{env:{HOME:'/tmp',LANG:'C.UTF-8',PATH:'/usr/bin'},stdio:['ignore','ignore','ignore','pipe','pipe']});
  child.on('error',()=>emit({kind:'error',error:'Chromium startup failed'}));
  child.on('exit',code=>{emit({kind:'exit',code});proxy.close();process.exitCode=code||0;input.close();process.stdin.destroy();});
  const frames=new NullFrameReader(maximumFrame);
  child.stdio[4].on('data',chunk=>{
    frames.push(chunk,frame=>{
      const value=JSON.parse(frame.toString());
      if(privateResponses.has(value.id)){
        const purpose=privateResponses.get(value.id);privateResponses.delete(value.id);
        if(purpose==='page_state'){
          const pages=value.result?.targetInfos?.filter(target=>target.type==='page');
          if(value.error||pages?.length!==1)emit({kind:'error',error:'Single structured page target required'});
          else emit({kind:'page_state',url:pages[0].url});
        }
        return;
      }
      if(value.method==='Page.javascriptDialogOpening'){
        const id=privateSequence--;privateResponses.set(id,'dialog');
        child.stdio[3].write(`${JSON.stringify({id,sessionId:value.sessionId,method:'Page.handleJavaScriptDialog',params:{accept:false}})}\0`);
      }
      emit(cdpPacket(value));
    });
  });
  emit({kind:'ready',mode});
}
async function startClient(packet) {
  if (typeof packet.token!=='string'||packet.token.length!==64) throw new Error('invalid private endpoint token');
  control=new WebSocketServer({host:'127.0.0.1',port:0,maxPayload:maximumFrame});
  control.on('connection',(socket,request)=>{
    if(request.url!==`/${packet.token}` || peer){socket.close();return;}
    peer=socket;
    socket.on('message',data=>emit(cdpPacket(JSON.parse(data.toString()))));
  });
  await new Promise(resolve=>control.on('listening',resolve));
  if(mode==='mcp'){
    child=spawn('/runtime/node',['/runtime/package/node_modules/@playwright/mcp/cli.js',
      '--cdp-endpoint',`ws://127.0.0.1:${control.address().port}/${packet.token}`,
      '--image-responses','omit','--timeout-navigation','20000','--timeout-action','5000',
      '--output-dir','/tmp/mcp-output','--output-max-size','1048576'],
      {env:{HOME:'/tmp',LANG:'C.UTF-8',PATH:'/usr/bin'},stdio:['pipe','pipe','ignore']});
    child.on('error',()=>emit({kind:'error',error:'Private MCP startup failed'}));
    child.on('exit',code=>{emit({kind:'exit',code});terminate();input.close();process.stdin.destroy();});
    const rpc=readline.createInterface({input:child.stdout,crlfDelay:Infinity});
    rpc.on('line',line=>{
      if(Buffer.byteLength(line)>1024*1024){terminate();return;}
      emit({kind:'mcp',value:JSON.parse(line)});
    });
    emit({kind:'ready',mode});return;
  }
  browser=await chromium.connectOverCDP(`ws://127.0.0.1:${control.address().port}/${packet.token}`,{timeout:20_000});
  const context=browser.contexts()[0];
  for(const page of context.pages())page.on('dialog',async dialog=>{dialogs++;await dialog.dismiss().catch(()=>{});});
  context.on('page',page=>page.on('dialog',async dialog=>{dialogs++;await dialog.dismiss().catch(()=>{});}));
  if(mode==='session')session=await sessionHelper(browser,packet.policy,()=>{emit({kind:"error",error:"Single Session page required"});terminate();input.close();process.stdin.destroy();});
  emit({kind:'ready',mode});
}
async function observe(page,timeout=deadline(20000)) {
  const text=(await page.locator('body').innerText({timeout:timeout(20000)})).toWellFormed();
  if(Buffer.byteLength(text)>64*1024)throw new Error('text snapshot exceeds 64 KiB');
  return text;
}
async function check(packet) {
  const timeout=deadline(110000);
  const page=browser.contexts()[0].pages()[0];
  const rows=[];let outcome='infrastructure_failed',snapshot='',evidenceError=null;
  try {
    const response=await page.goto(packet.url,{waitUntil:'domcontentloaded',timeout:timeout(20000)});
    if([401,403].includes(response?.status()))outcome='target_unavailable';
    else {
      try{await page.getByText(packet.spec.entry_identity,{exact:true}).first().waitFor({state:'visible',timeout:timeout(5000)});}
      catch{outcome='target_unavailable';}
      if(outcome!=='target_unavailable'){
        for(const assertion of packet.spec.assertions){
          let passed=false;
          try{
            if(assertion.kind==='final_url')passed=page.url()===assertion.url;
            else if(assertion.kind==='text_visible'){await page.getByText(assertion.text,{exact:true}).first().waitFor({state:'visible',timeout:timeout(5000)});passed=true;}
            else if(assertion.kind==='role_visible'){await page.getByRole(assertion.role,{name:assertion.name,exact:true}).waitFor({state:'visible',timeout:timeout(5000)});passed=true;}
          }catch{}
          rows.push({assertion,passed,detail:passed?'Predicate satisfied':'Predicate not satisfied'});
        }
        outcome=rows.every(row=>row.passed)?'pass':'assertion_failed';
      }
    }
    snapshot=await observe(page,timeout);
  }catch(error){outcome=error.name==='TimeoutError'?'timeout':'infrastructure_failed';}
  try{
    await page.setViewportSize({width:1280,height:720});
    const png=await page.screenshot({type:'png',fullPage:false,timeout:timeout(20000)});
    if(png.length>512*1024)evidenceError='Screenshot exceeds 512 KiB';
    else emit({kind:'artifact',png:png.toString('base64')});
  }catch{evidenceError='Screenshot unavailable';}
  emit({kind:'checked',result:{outcome,final_url:page.url(),assertions:rows,snapshot,dialogs_dismissed:dialogs,evidence_error:evidenceError}});
}
