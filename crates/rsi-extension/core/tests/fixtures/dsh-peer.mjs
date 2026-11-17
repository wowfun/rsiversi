// The peer implementation comes from the explicitly selected independent repo.
import {pathToFileURL} from 'node:url';
const {Peer} = await import(pathToFileURL(process.argv[3]).href);
const {resolveBody} = await import(pathToFileURL(process.argv[4]).href);
const peer = new Peer({input:process.stdin,output:process.stdout,generation:process.argv[2],handler:async(method,payload,signal)=>{
  if (method === 'extension.hello') return {protocol:2};
  if (method === 'extension.ping') { await peer.request('ping',null); return null; }
  if (method === 'extension.close') return null;
  if (method === 'hold') { await peer.request('entered',null); return new Promise(resolve=>signal.addEventListener('abort',resolve,{once:true})); }
  if (method === 'pause-input') { process.stdin.pause(); setTimeout(()=>process.stdin.resume(),1200); return null; }
  if (method === 'echo') return payload;
  if (method === 'echo-body') {
    const maximum=(payload.expected==='tool'?4:8)*1024*1024;
    const value=await resolveBody(payload.body,maximum,(token,offset)=>peer.request('body.read',{token,offset},{signal}));
    const expected=payload.expected==='application-limit'?'x'.repeat(maximum-11):(payload.expected==='message'?'\n':'界').repeat(1024*1024);
    return value.text===expected;
  }
  throw Error('unknown fixture request');
}});
