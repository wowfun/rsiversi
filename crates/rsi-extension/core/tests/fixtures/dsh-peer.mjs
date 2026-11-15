// The peer implementation comes from the explicitly selected independent repo.
import {pathToFileURL} from 'node:url';
const {Peer} = await import(pathToFileURL(process.argv[3]).href);
const peer = new Peer({input:process.stdin,output:process.stdout,generation:process.argv[2],handler:async(method,payload,signal)=>{
  if (method === 'extension.hello') return {protocol:2};
  if (method === 'extension.ping') { await peer.request('ping',null); return null; }
  if (method === 'extension.close') return null;
  if (method === 'hold') { await peer.request('entered',null); return new Promise(resolve=>signal.addEventListener('abort',resolve,{once:true})); }
  if (method === 'pause-input') { process.stdin.pause(); setTimeout(()=>process.stdin.resume(),1200); return null; }
  if (method === 'echo') return payload;
  throw Error('unknown fixture request');
}});
