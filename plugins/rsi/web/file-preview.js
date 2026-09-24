import { mount as mountStandard } from './standard.js';
import { boundedImage, imageBudget, maximumImagePixels } from './image-bounds.js';
import { createHighlighterCoreSync } from 'shiki/core';
import { createJavaScriptRegexEngine } from 'shiki/engine/javascript';
import theme from 'shiki/themes/github-light.mjs';
import rust from 'shiki/langs/rust.mjs';
import typescript from 'shiki/langs/typescript.mjs';
import javascript from 'shiki/langs/javascript.mjs';
import python from 'shiki/langs/python.mjs';
import json from 'shiki/langs/json.mjs';
import yaml from 'shiki/langs/yaml.mjs';
import toml from 'shiki/langs/toml.mjs';
import shell from 'shiki/langs/shellscript.mjs';
import html from 'shiki/langs/html.mjs';
import css from 'shiki/langs/css.mjs';

const maximum = 32 * 1024 * 1024;
let retained = 0, highlighter;
const decoder = new TextDecoder('utf-8', { fatal: true });
const aliases = { rs:'rust',ts:'typescript',tsx:'typescript',js:'javascript',jsx:'javascript',py:'python',json:'json',yaml:'yaml',yml:'yaml',toml:'toml',sh:'shellscript',bash:'shellscript',html:'html',htm:'html',css:'css' };
function node(tag, cls, text) { const item=document.createElement(tag); if(cls)item.className=cls;if(text!==undefined)item.textContent=text;return item; }
function language(label) { const suffix=label.toLowerCase().split('.').pop();return aliases[suffix]??suffix; }
function code(text, lang, numbered=true) {
  const pre=node('pre','file-code'), content=node('code');pre.append(content);pre.dataset.language=lang;
  // Very long lines and large documents remain inspectable without synchronous regex work.
  let tokens;
  if(text.length<=256*1024 && text.split('\n').every(line=>line.length<=4096)) {
    highlighter ??= createHighlighterCoreSync({themes:[theme],langs:[rust,typescript,javascript,python,json,yaml,toml,shell,html,css],engine:createJavaScriptRegexEngine()});
    if(highlighter.getLoadedLanguages().includes(aliases[lang]??lang)) tokens=highlighter.codeToTokens(text,{lang:aliases[lang]??lang,theme:'github-light'}).tokens;
  }
  const lines=tokens??text.split('\n').map(line=>[{content:line}]);
  if(lines.length>20000){content.textContent=text;return pre;}
  for(const [index,line] of lines.entries()) {
    const row=node('span','file-code-line');
    if(numbered){const number=node('span','file-line-number',String(index+1));number.setAttribute('aria-hidden','true');row.append(number);}
    const value=node('span','file-code-content');
    for(const token of line){const part=node('span','file-token',token.content);if(token.color&&/^#[0-9a-f]{6,8}$/i.test(token.color))part.style.color=token.color;value.append(part);}
    row.append(value);content.append(row);if(index+1<lines.length)content.append(document.createTextNode('\n'));
  }
  return pre;
}
function markdown(source, assets) {
  const parsed=new DOMParser().parseFromString(source,'text/html');
  const allowed=new Set(['P','H1','H2','H3','H4','H5','H6','BLOCKQUOTE','PRE','CODE','UL','OL','LI','STRONG','EM','DEL','S','A','IMG','HR','BR','TABLE','THEAD','TBODY','TR','TH','TD','INPUT']);
  let count=0;
  function copy(item,depth=0) {
    if(++count>65536||depth>40)throw new Error('Markdown exceeds its DOM budget');
    if(item.nodeType===Node.TEXT_NODE)return document.createTextNode(item.textContent);
    if(item.nodeType!==Node.ELEMENT_NODE)return document.createTextNode('');
    if(!allowed.has(item.tagName))return document.createTextNode(item.textContent);
    if(item.tagName==='PRE'&&item.firstElementChild?.tagName==='CODE')return code(item.textContent,item.firstElementChild.className.replace(/^language-/,''),false);
    const result=node(item.tagName.toLowerCase());
    if(item.tagName==='A') {const href=item.getAttribute('href')??'';if(/^https?:\/\//.test(href)){result.href=href;result.target='_blank';result.rel='noopener noreferrer';}else if(href.startsWith('#'))result.href=href;}
    if(item.tagName==='IMG') {const match=/^rsi-preview-resource:(\d+)(#.*)?$/.exec(item.getAttribute('src')??'');result.alt=item.getAttribute('alt')??'';if(match&&assets.has(`asset-${match[1]}`))result.src=assets.get(`asset-${match[1]}`)+(match[2]??'');else return node('span','file-resource-error',`[${result.alt||'Image unavailable'}]`);result.addEventListener('error',()=>result.replaceWith(node('span','file-resource-error',`[${result.alt||'Image unavailable'}]`)),{once:true});}
    if(item.tagName==='INPUT'){result.type='checkbox';result.disabled=true;result.checked=item.hasAttribute('checked');}
    if(item.tagName==='OL'&&/^\d+$/.test(item.getAttribute('start')??''))result.start=Number(item.getAttribute('start'));
    for(const child of item.childNodes)result.append(copy(child,depth+1));return result;
  }
  const result=node('article','file-markdown');for(const child of parsed.body.childNodes)result.append(copy(child));return result;
}
export async function mount(root,initial,host,signal) {
  let revision, current, bytes=new Map(), urls=[], allocation=0, epoch=0, frame, mode='preview', online=false, wrap=false;
  let frameEvents=new AbortController(), events=new AbortController(), fallbackRenderer, fallbackError;
  let fallbackEvents=new AbortController();
  function closeFrame(){frameEvents.abort();frameEvents=new AbortController();frame?.remove();frame=undefined;}
  function clear(){++epoch;closeFrame();fallbackEvents.abort();fallbackEvents=new AbortController();fallbackRenderer=undefined;fallbackError=undefined;events.abort();events=new AbortController();for(const url of urls)URL.revokeObjectURL(url);urls=[];bytes.clear();retained-=allocation;allocation=0;}
  function blob(data,mime,admitPixels){const url=URL.createObjectURL(new Blob([mime.startsWith('image/')?boundedImage(data,mime,admitPixels):data],{type:mime}));urls.push(url);return url;}
  function fail(error){if(!signal.aborted)root.replaceChildren(node('p','source-error',error.message));}
  async function read(entry,version) {
    const data=new Uint8Array(entry.bytes);let offset=0;
    while(offset<data.length){if(signal.aborted||version!==epoch)throw new Error('Preview retired');const page=await host.source(entry.source,offset,Math.min(64*1024,data.length-offset));if(!page.length)throw new Error('Unexpected end of preview source');data.set(page,offset);offset+=page.length;}
    return data;
  }
  function button(label,action,cls='quiet'){const item=node('button',cls,label);item.type='button';item.addEventListener('click',()=>{try{Promise.resolve(action()).catch(fail);}catch(error){fail(error);}},{signal:events.signal});return item;}
  function showFrame(container,data) {
    closeFrame();frame=document.createElement('iframe');frame.className='file-html';frame.title=`HTML preview: ${data.label}`;frame.setAttribute('sandbox','allow-scripts');frame.referrerPolicy='no-referrer';
    const expected=online?'online':'local', view=frame, version=epoch;
    let transferred=false;
    window.addEventListener('message',event=>{
      if(event.source!==view.contentWindow||event.data?.type!=='rsi-preview-ready'||event.data?.mode!==expected||transferred||version!==epoch)return;
      transferred=true;
      const channel=new MessageChannel();
      channel.port1.onmessage=event=>{if(version!==epoch)return;if(event.data?.type==='error')container.prepend(node('p','source-error',String(event.data.message).slice(0,1024)));channel.port1.close();};
      const assets=data.sources.filter(item=>item.name.startsWith('asset-')).map(item=>({name:item.name,mime:item.media_type,data:bytes.get(item.name)}));
      view.contentWindow.postMessage({type:'rsi-preview-connect'},'*',[channel.port2]);
      channel.port1.postMessage({type:'render',html:decoder.decode(bytes.get('document')),assets});
      frameEvents.signal.addEventListener('abort',()=>channel.port1.close(),{once:true});
    },{signal:frameEvents.signal});
    frame.src=online?'/preview-online.html':'/preview-local.html';container.append(frame);
  }
  function render() {
    closeFrame();for(const url of urls)URL.revokeObjectURL(url);urls=[];events.abort();events=new AbortController();
    const data=current.model.data, body=node('section','file-preview ui-contribution');body.dataset.previewKind=data.kind;
    const bar=node('div','file-preview-toolbar');bar.append(node('strong','file-preview-title',data.label));
    if(data.kind==='html'||data.kind==='markdown'||(data.kind==='image'&&data.label.endsWith('.svg'))){
      const preview=button('Preview',()=>{mode='preview';render();}),source=button('Source',()=>{mode='source';render();});preview.setAttribute('aria-pressed',String(mode==='preview'));source.setAttribute('aria-pressed',String(mode==='source'));bar.append(preview,source);
    }
    if(data.kind!=='image') {
      const copy=button('Copy source',()=>host.clipboard(decoder.decode(bytes.get('main'))));
      copy.disabled=bytes.get('main').length>64*1024;if(copy.disabled)copy.title='Copy supports source up to 64 KiB';bar.append(copy);
    }
    if(data.kind==='code'||mode==='source')bar.append(button(wrap?'No wrap':'Wrap lines',()=>{wrap=!wrap;render();}));
    for(const item of current.model.standard_view?.elements??[]){if(item.kind==='button'&&['Refresh','Release snapshot','Workspace root'].includes(item.label))bar.append(button(item.label,()=>host.invoke(item.action,{value:item.value,fields:{}})));}
    if(data.kind==='html'&&mode==='preview'){
      const toggle=button(online?'HTTPS resources: on':'Enable HTTPS resources',()=>{online=!online;render();});toggle.setAttribute('aria-pressed',String(online));bar.append(toggle);body.append(node('p','file-network-trust','Enabling HTTPS lets this document and remote scripts send previewed contents to any HTTPS server.'));
    }
    body.append(bar);
    const path=current.model.standard_view?.elements.find(item=>item.kind==='field'&&item.label==='Path bytes (hex)');
    if(path){const identity=node('details','file-path');identity.append(node('summary','','File path bytes'),node('code','',path.value));body.append(identity);}
    for(const message of data.diagnostics??[])body.append(node('p','file-resource-error',message));
    const content=node('div','file-preview-body');body.append(content);root.replaceChildren(body);
    if(mode==='source'||data.kind==='code'){const value=code(decoder.decode(bytes.get('main')),language(data.label));value.classList.toggle('wrap',wrap);content.append(value);}
    else if(data.kind==='markdown'){
      const images=new Map(),admitPixels=imageBudget();for(const item of data.sources.filter(item=>item.name.startsWith('asset-'))){
        try { if(!item.media_type.startsWith('image/'))throw new Error('Unsupported image type');images.set(item.name,blob(bytes.get(item.name),item.media_type,admitPixels)); }
        catch(error){content.append(node('p','file-resource-error',error.message));}
      }
      content.append(markdown(decoder.decode(bytes.get('document')),images));
    }else if(data.kind==='image'){
      const image=node('img','file-image');image.alt=data.label;image.src=blob(bytes.get('main'),data.sources[0].media_type);let scale;
      const size=node('span','hint');bar.append(size);
      const resize=()=>{image.classList.toggle('fit',scale===undefined);image.style.width=scale===undefined?'':`${image.naturalWidth*scale}px`;size.textContent=scale===undefined?'Fit':`${Math.round(scale*100)}%`;};
      bar.append(button('Fit',()=>{scale=undefined;resize();}),button('100%',()=>{scale=1;resize();}),button('Zoom in',()=>{scale=Math.min(8,(scale??Math.min(1,content.clientWidth/image.naturalWidth))*1.25);resize();}),button('Zoom out',()=>{scale=Math.max(.05,(scale??1)/1.25);resize();}));
      image.addEventListener('load',()=>{if(image.naturalWidth*image.naturalHeight>maximumImagePixels){image.remove();content.append(node('p','source-error','Image exceeds the pixel limit'));}else resize();},{once:true});image.addEventListener('error',()=>{image.remove();content.append(node('p','source-error','Image could not be decoded'));},{once:true});image.classList.add('fit');content.append(image);
    }else if(data.kind==='html')showFrame(content,data);
  }
  async function update(snapshot) {
    current=snapshot;const data=snapshot.model.data;
    if(snapshot.model.schema.name!=='rsi.file-preview'||snapshot.model.schema.version!==1||!['code','markdown','image','html'].includes(data?.kind))throw new Error('Invalid file preview model');
    if(revision===data.revision){if(fallbackRenderer)await fallbackRenderer.update({...snapshot,error:fallbackError});return;}
    clear();revision=data.revision;mode='preview';online=false;const version=epoch;
    root.replaceChildren(node('p','hint','Loading file preview…'));
    let total=0;
    if(!Array.isArray(data.sources)||data.sources.length>32)throw new Error('Invalid preview sources');
    for(const entry of data.sources){if(!Number.isSafeInteger(entry.bytes)||entry.bytes<0||!snapshot.model.sources.some(source=>source.name===entry.source&&source.media_type===entry.media_type)||typeof entry.name!=='string')throw new Error('Invalid preview source');total+=entry.bytes;}
    if(total>maximum-retained)throw new Error('Preview exceeds the shared 32 MiB byte budget');retained+=total;allocation=total;
    try {
      for(const entry of data.sources){const data=await read(entry,version);if(version!==epoch||signal.aborted)return;bytes.set(entry.name,data);}
      render();
    }catch(error){if(version!==epoch||signal.aborted)return;clear();fallbackError=error.message;fallbackRenderer=await mountStandard(root,{...snapshot,error:fallbackError},host,fallbackEvents.signal);}
  }
  let staged=initial, pending;
  signal.addEventListener('abort',clear,{once:true});
  root.replaceChildren(node('p','hint','Loading file preview…'));
  return {
    update(snapshot){staged=snapshot;},
    activate(){const next=staged;staged=undefined;if(next)pending=update(next).catch(fail);},
    async dispose(){clear();await pending;root.replaceChildren();}
  };
}
