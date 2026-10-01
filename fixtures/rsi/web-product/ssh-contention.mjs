import assert from 'node:assert/strict';
import net from 'node:net';

// The existing target uses real SSH. A second isolated TCP endpoint deliberately
// stalls before SSH negotiation so unrelated product operations can be observed.
export async function verifyTargetIsolation({remote,local,epoch,device,endpoint,hostKey,fingerprint,identity,currentRequest}) {
  let accepted;
  const entered=new Promise(resolve=>accepted=resolve),sockets=new Set();
  const listener=net.createServer(socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));accepted()});
  await new Promise(resolve=>listener.listen(0,'127.0.0.1',resolve));
  let pending,timer;
  try {
    const create=async(target,port)=>{
      const response=await remote('put-candidate',{host_epoch:epoch,expected:'0',candidate:{target,name:'Contention fixture',endpoint:{...endpoint,port}}});
      assert.equal(response.status(),200);return response.json();
    };
    let stalled=await create('f'.repeat(32),listener.address().port);
    stalled=local({operation:'ssh_trust',request:{selection:{host_epoch:epoch,target:stalled.candidate.target,revision:stalled.revision},host_key:hostKey,fingerprint,identity_path:identity}});
    local({operation:'set_grant',request:{expected:local({operation:'grants'}).revision,scope:{principal:{kind:'device',id:device},scope:{kind:'ssh_use',target:stalled.candidate.target}},granted:true}});
    const request={selection:{host_epoch:epoch,target:stalled.candidate.target,revision:stalled.revision},expected_connection_epoch:null};
    pending=remote('connect',request).then(reply=>({reply}),error=>({error}));
    await Promise.race([entered,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('Stalled target never reached its isolated TCP endpoint')),15000)})]);clearTimeout(timer);
    const same=await remote('connect',request);assert.equal(same.status(),422);assert.equal((await same.json()).kind,'busy');
    const denied=await remote('put-candidate',{host_epoch:epoch,expected:stalled.revision,candidate:stalled.candidate});
    assert([401,403].includes(denied.status()),'Manage denial must precede target Busy disclosure');
    const started=performance.now();
    const other=await create('e'.repeat(32),endpoint.port);
    assert.equal(other.revision,'1');
    const directory=local({operation:'ssh_resolve',request:currentRequest});assert.equal(directory.path,currentRequest.path);
    assert(performance.now()-started<5000,'Unrelated target operations waited for the stalled handshake');
    for(const socket of sockets)socket.destroy();
    const result=await pending;assert(!result.error);assert.equal(result.reply.status(),422);assert.equal((await result.reply.json()).kind,'connection_failed');
    return 'stalled SSH handshake preserves unrelated candidate/directory operations and reports same-target Busy';
  } finally {
    clearTimeout(timer);for(const socket of sockets)socket.destroy();
    await new Promise(resolve=>listener.close(resolve));await pending;
  }
}
