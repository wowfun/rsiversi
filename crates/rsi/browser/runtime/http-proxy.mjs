// The namespace contains no host network. Rust admits and pins each destination.
// Parse one bounded proxy header, then transfer raw HTTP/WS bytes with credits;
// never decode or reconstruct request bodies, including chunked framing.
import net from 'node:net';
function validHeaders(lines){
  return lines.every(line=>{
    const colon=line.indexOf(':');
    return colon>0&&/^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/.test(line.slice(0,colon))
      &&!/[\x00-\x08\x0a-\x1f\x7f]/.test(line);
  });
}
// Geometric growth bounds total copying; delimiter searches overlap only the
// previous three bytes, including a split final delimiter.
export class HeaderAccumulator {
  constructor(maximum=16384+4+32768){this.maximum=maximum;this.buffer=Buffer.alloc(0);this.length=0;}
  push(chunk){
    if(chunk.length>this.maximum-this.length)throw new Error('proxy initial bytes exceed bound');
    if(this.length+chunk.length>this.buffer.length){
      const buffer=Buffer.allocUnsafe(Math.min(this.maximum,Math.max(1024,this.buffer.length*2,this.length+chunk.length)));
      this.buffer.copy(buffer,0,0,this.length);this.buffer=buffer;
    }
    const previous=this.length;chunk.copy(this.buffer,this.length);this.length+=chunk.length;
    const end=this.buffer.subarray(0,this.length).indexOf('\r\n\r\n',Math.max(0,previous-3));
    if(end<0){if(this.length>16384+3)throw new Error('proxy header exceeds bound');return null;}
    if(end>16384||this.length-end-4>32768)throw new Error('proxy initial bytes exceed bound');
    return {bytes:this.buffer.subarray(0,this.length),end};
  }
}
export function createProxy(admit,local=null) {
  return net.createServer(socket => {
    const incoming=new HeaderAccumulator();
    const timer=setTimeout(()=>socket.destroy(),5000);
    const fail=()=>{clearTimeout(timer);socket.destroy();};
    const read=chunk=>{
      let parsed;try{parsed=incoming.push(chunk);}catch{fail();return;}
      if(!parsed)return;
      const {bytes:header,end}=parsed;
      socket.pause();socket.removeListener('data',read);clearTimeout(timer);
      try {
        const lines=header.subarray(0,end).toString('latin1').split('\r\n');
        const request=lines.shift();
        const match=/^([A-Z]+) ([\x21-\x7e]+) HTTP\/1\.[01]$/.exec(request);
        if(!match||/[^\x20-\x7e]/.test(request)||!validHeaders(lines))throw new Error('invalid proxy request or headers');
        const [,method,raw]=match;
        const target=new URL(method==='CONNECT'?`https://${raw}`:raw);
        if(target.username||target.password||!['http:','https:','ws:'].includes(target.protocol))throw new Error('anonymous proxy URL required');
        const connect=method==='CONNECT';
        if(connect&&(target.pathname!=='/'||target.search||target.hash||raw!==`${target.hostname}:${target.port||443}`))throw new Error('invalid CONNECT authority');
        if(!connect&&target.protocol!=='http:'&&target.protocol!=='ws:')throw new Error('HTTPS requires CONNECT');
        const host=target.hostname,port=Number(target.port||(connect?443:80));
        let head=header.subarray(end+4),transport='connect';
        // Chromium tunnels ws through CONNECT. Terminate that local handshake,
        // then validate an HTTP WebSocket upgrade before admitting any egress.
        if(connect&&local&&host===local.hostname&&port===Number(local.port||80)){
          if(head.length)throw new Error('local WS handshake must follow CONNECT');
          socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
          const handshakeInput=new HeaderAccumulator(32768);const expires=setTimeout(()=>socket.destroy(),5000);
          const upgrade=chunk=>{
            let parsed;try{parsed=handshakeInput.push(chunk);}catch{socket.destroy();return;}
            if(!parsed)return;
            const {bytes:handshake,end:boundary}=parsed;
            clearTimeout(expires);socket.pause();socket.removeListener('data',upgrade);
            const lines=handshake.subarray(0,boundary).toString('latin1').split('\r\n');
            const hosts=lines.slice(1).filter(line=>/^host:/i.test(line));
            if(!/^GET \/[\x21-\x7e]* HTTP\/1\.1$/.test(lines[0])||/[^\x20-\x7e]/.test(lines[0])||!validHeaders(lines.slice(1))||hosts.length!==1||hosts[0].slice(5).trim()!==local.host||!lines.some(line=>/^upgrade:\s*websocket\s*$/i.test(line))||!lines.some(line=>/^connection:.*\bupgrade\b/i.test(line))){socket.destroy();return;}
            admit({socket,host,port,transport:'ws',head:handshake,connect:false});
          };
          socket.on('data',upgrade);socket.once('close',()=>clearTimeout(expires));socket.resume();return;
        }
        if(!connect){
          const filtered=[];let hostCount=0,upgrade=false;
          for(const line of lines){
            const colon=line.indexOf(':');
            const key=line.slice(0,colon).toLowerCase(),value=line.slice(colon+1).trim();
            if(key==='host'){hostCount++;if(value!==target.host)throw new Error('Host authority mismatch');}
            if(key==='upgrade')upgrade=value.toLowerCase()==='websocket';
            if(!['proxy-authorization','proxy-connection','connection'].includes(key))filtered.push(line);
          }
          if(hostCount!==1)throw new Error('one Host header required');
          transport=upgrade?'ws':'http';
          filtered.push(upgrade?'Connection: Upgrade':'Connection: close');
          head=Buffer.concat([Buffer.from(`${method} ${target.pathname}${target.search} HTTP/1.1\r\n${filtered.join('\r\n')}\r\n\r\n`,'latin1'),head]);
          if(head.length>32768)throw new Error('proxy initial bytes exceed credit');
        }
        admit({socket,host,port,transport,head,connect});
      }catch{fail();}
    };
    socket.on('data',read);socket.on('error',()=>{});socket.on('close',()=>clearTimeout(timer));
  });
}
