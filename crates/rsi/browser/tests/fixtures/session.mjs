import http from 'node:http';
import {WebSocketServer} from '../../runtime/node_modules/ws/wrapper.mjs';
const server=http.createServer((request,response)=>{
  if(request.url==='/redirect'){response.writeHead(302,{location:'http://localhost:'+server.address().port+'/'}).end();return;}
  response.setHeader('content-type','text/html; charset=utf-8');
  response.end(`<!doctype html><html><head><title>Session browser fixture</title><style>body{margin:40px;font:20px system-ui;background:#f2f5f7;color:#173042}input,button{padding:12px;margin:10px}h1{font-size:36px}output{display:block;padding:20px;background:white}</style></head><body><h1>Shared Session browser</h1><label>Name <input aria-label="Name"></label><button id="apply">Apply</button><button id="replace">Replace node</button><output id="result">Ready</output><p id="ws">WS connecting</p><script>
const input=document.querySelector('input');document.querySelector('#apply').onclick=()=>document.querySelector('#result').textContent='Hello '+input.value;
document.querySelector('#replace').onclick=()=>{setTimeout(()=>{const old=document.querySelector('#apply');old.replaceWith(old.cloneNode(true));},100);};
const ws=new WebSocket('ws://'+location.host+'/socket');ws.onopen=()=>ws.send('fixture');ws.onmessage=event=>document.querySelector('#ws').textContent=event.data;
if(location.pathname==='/unicode'){const button=document.createElement('button');button.setAttribute('aria-label','Unicode '+String.fromCharCode(0xd800));button.textContent='Unicode fixture';document.body.append(button,document.createTextNode('Scalar '+String.fromCharCode(0xdfff)));}
</script></body></html>`);
});
const ws=new WebSocketServer({server,path:'/socket'});ws.on('connection',socket=>socket.on('message',()=>socket.send('WS verified')));
server.listen(0,'127.0.0.1',()=>process.stdout.write(String(server.address().port)+'\n'));
