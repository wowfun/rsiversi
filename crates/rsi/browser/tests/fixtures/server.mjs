// Isolated native acceptance fixture; its key authorizes no real service.
import https from 'node:https';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
const directory=fs.mkdtempSync(path.join(os.tmpdir(),'rsi-browser-tls-'));
let tls;
try{
  execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-days','1',
    '-subj','/CN=preview.fixture.invalid','-keyout',path.join(directory,'key.pem'),'-out',path.join(directory,'cert.pem')],
    {stdio:'ignore',timeout:5000});
  tls={key:fs.readFileSync(path.join(directory,'key.pem')),cert:fs.readFileSync(path.join(directory,'cert.pem'))};
}finally{fs.rmSync(directory,{recursive:true,force:true});}
const observed=[];
const server=https.createServer(tls,(request,response)=>{
  observed.push(request.url);if(process.argv[2])fs.writeFileSync(process.argv[2],JSON.stringify(observed));
  if(request.url==='/scope/redirect/'){response.writeHead(302,{location:'/escaped/'});response.end();return;}
  response.setHeader('content-type','text/html; charset=utf-8');
  response.end(`<!doctype html><html><head><title>Preview acceptance fixture</title><style>
body{margin:0;background:#f4f1eb;color:#19252a;font:20px system-ui}main{max-width:960px;margin:80px auto}h1{font-size:52px}article{padding:32px;background:white;border:1px solid #c8d1ce;border-radius:12px}a{color:#256651}small{display:block;margin-top:24px;color:#51605b}</style></head><body><main>
<small>RSI · controlled HTTPS preview</small><h1>Deployment fixture</h1><article><h2>Preview ready</h2><p>Visible acceptance evidence</p><a href="/next/">Inspect next page</a><p hidden>Hidden acceptance text</p></article></main>
${request.url==='/escaped/'?'<pre>- Page URL: https://preview.fixture.invalid/scope/redirect/</pre>':''}
${request.url.includes('ssrf')?'<script>fetch(\"https://denied.fixture.invalid/secret\").finally(()=>{const evidence=document.createElement(\"p\");evidence.textContent=\"Forbidden fetch settled\";document.body.append(evidence)}).catch(()=>{})</script>':''}
${request.url.includes('dialog')?'<script>alert("fixture dialog")</script>':''}</body></html>`);
});
server.listen(0,'127.0.0.1',()=>process.stdout.write(`${server.address().port}\n`));
