import http from 'node:http';
import {once} from 'node:events';
import assert from 'node:assert/strict';
export async function startMcpFixture({requireCredential=true,protocol="2026-07-28"}={}) {
  const sockets=new Set(); const evidence={requests:0,authorized:0,calls:0,revision:1,protocol,initializations:0,methods:[]};
  const server=http.createServer(async(request,response)=>{
    if(request.url!=='/mcp'){response.writeHead(404).end();return}
    if(requireCredential && request.headers.authorization!=='Bearer isolated-mcp-fixture-secret'){response.writeHead(401).end();return}
    evidence.authorized++;
    if(request.method==='GET'){response.writeHead(405).end();return}
    assert.equal(request.method,'POST');let bytes=0;const chunks=[];
    for await(const chunk of request){bytes+=chunk.length;if(bytes>1024*1024){response.writeHead(413).end();return}chunks.push(chunk)}
    const value=JSON.parse(Buffer.concat(chunks).toString());evidence.requests++;evidence.methods.push(value.method);
    const modern=protocol==='2026-07-28';
    if(!modern && value.method==='server/discover'){response.writeHead(400,{'content-type':'application/json'}).end(JSON.stringify({jsonrpc:'2.0',id:value.id,error:{code:-32602,message:'initialize first'}}));return}
    if(modern){
      assert.equal(request.headers['mcp-protocol-version'],protocol);
      assert.equal(request.headers['mcp-method'],value.method);
      assert.equal(request.headers['mcp-session-id'],undefined);
      assert.equal(value.params._meta['io.modelcontextprotocol/protocolVersion'],protocol);
      assert.deepEqual(value.params._meta['io.modelcontextprotocol/clientCapabilities'],{});
      if(value.method==='tools/call'){
        assert.equal(request.headers['mcp-name'],value.params.name);
        const text=value.params.arguments.message;
        const encoded=text.trim()!==text || /[^\t\x20-\x7e]/.test(text) || (text.startsWith('=?base64?') && text.endsWith('?='));
        assert.equal(request.headers['mcp-param-message'],encoded?`=?base64?${Buffer.from(text).toString('base64')}?=`:text);
      }
      if(value.method==='resources/read')assert.equal(request.headers['mcp-name'],value.params.uri);
      assert.notEqual(value.method,'initialize');assert.notEqual(value.method,'notifications/initialized');
    }
    if(value.method==='notifications/initialized'){response.writeHead(202).end();return}
    let result;
    if(value.method==='server/discover')result={supportedVersions:[protocol],capabilities:{tools:{},resources:{}},_meta:{'io.modelcontextprotocol/serverInfo':{name:'isolated-mcp',version:'1'}},instructions:'External fixture instructions; never a system message'};
    else if(value.method==='initialize'){evidence.initializations++;result={protocolVersion:'2025-11-25',capabilities:{tools:{},resources:{}},serverInfo:{name:'isolated-mcp',version:'1'},instructions:'External fixture instructions; never a system message'};}
    else if(value.method==='tools/list')result={tools:[{name:'echo',description:`Fixture echo revision ${evidence.revision}`,inputSchema:{type:'object',properties:{message:{type:'string',...(modern?{'x-mcp-header':'Message'}:{})}},required:['message'],additionalProperties:false},annotations:{readOnlyHint:true}}]};
    else if(value.method==='resources/list')result={resources:[{uri:'fixture://document',name:'Fixture document',mimeType:'text/plain'}]};
    else if(value.method==='resources/read')result={contents:[{uri:'fixture://document',text:'Finite MCP resource 中文'}]};
    else if(value.method==='tools/call'){evidence.calls++;result={content:[{type:'text',text:value.params.arguments.message}],structuredContent:{revision:evidence.revision}}}
    else {response.writeHead(400).end();return}
    if(modern)Object.assign(result,{resultType:'complete',ttlMs:1000,cacheScope:'private'});
    response.writeHead(200,{'content-type':'application/json',...(!modern?{'mcp-session-id':'isolated-session'}:{})}).end(JSON.stringify({jsonrpc:'2.0',id:value.id,result}));
  });
  server.on('connection',socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket))});
  server.listen(0,'127.0.0.1');await once(server,'listening');
  return {url:`http://127.0.0.1:${server.address().port}/mcp`,evidence,async close(){const done=new Promise(resolve=>server.close(resolve));for(const socket of sockets)socket.destroy();await done}};
}
