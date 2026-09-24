import { boundedResource, imageBudget } from './image-bounds.js';
// Runs only in a response-sandboxed immutable document. No application bridge exists here.
(() => {
  const mode=location.pathname==='/preview-online.html'?'online':'local';
  const parentOrigin=location.protocol+'//'+location.host;
  let connected=false;
  window.addEventListener('message',event=>{
    if(connected||event.source!==parent||event.origin!==parentOrigin||event.data?.type!=='rsi-preview-connect'||event.ports.length!==1)return;
    connected=true;const port=event.ports[0];
    port.onmessage=event=>{
      try {
        const value=event.data;
        if(value?.type!=='render'||typeof value.html!=='string'||value.html.length>8*1024*1024||!Array.isArray(value.assets)||value.assets.length>30)throw new Error('Invalid HTML preview package');
        let total=value.html.length;const resources=new Map(),urls=new Map(),decoder=new TextDecoder('utf-8',{fatal:true}),admitPixels=imageBudget();
        for(const asset of value.assets){if(!/^asset-\d+$/.test(asset.name)||!(asset.data instanceof Uint8Array)||typeof asset.mime!=='string'||resources.has(asset.name))throw new Error('Invalid preview resource');total+=asset.data.byteLength;resources.set(asset.name,asset);}
        if(total>32*1024*1024)throw new Error('HTML package exceeds its byte budget');
        function rewrite(text,stack){return text.replace(/rsi-preview-resource:(\d+)/g,(_,index)=>url(`asset-${index}`,stack));}
        function url(name,stack=new Set()){
          if(urls.has(name))return urls.get(name);
          if(stack.has(name))throw new Error('Cyclic CSS resource dependency');
          // Rust reports unreadable references and omits their source bytes.
          const asset=resources.get(name);if(!asset)return 'about:blank';
          const nested=new Set(stack);nested.add(name);
          const admitted=boundedResource(asset.data,asset.mime,admitPixels);
          const data=asset.mime==='text/css'?rewrite(decoder.decode(admitted),nested):admitted;
          const result=URL.createObjectURL(new Blob([data],{type:asset.mime}));urls.set(name,result);return result;
        }
        const documentText=rewrite(value.html,new Set());
        // Close the sole data channel before any user-authored script can execute.
        port.postMessage({type:'ready'});port.close();
        document.open();document.write(documentText);document.close();
      }catch(error){document.body.textContent='HTML preview unavailable';port.postMessage({type:'error',message:String(error.message).slice(0,1024)});port.close();}
    };
    port.start();
  });
  parent.postMessage({type:'rsi-preview-ready',mode},'*');
})();
