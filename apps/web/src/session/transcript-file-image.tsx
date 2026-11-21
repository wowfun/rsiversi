import {useEffect,useRef,useState} from 'react';
import {input} from '../bridge.ts';
import {useOptionalDocumentSession} from '../document-plugins.tsx';
import {boundedImage} from '../../image-bounds.js';
import {MarkdownImage} from './markdown-image.tsx';
import css from './markdown-image.module.css';

type Props = {path:string; alt:string; linked:boolean};
const waiting=new Set<()=>Promise<void>>();
let active=0;
function drain() {
  while (active<2 && waiting.size) {
    const next=waiting.values().next().value!;
    waiting.delete(next);
    active++;
    void next().finally(()=>{active--;drain()});
  }
}
function FileImage({pane,generation,path,alt,linked}:Props & {pane:string;generation:string}) {
  const placeholder=useRef<HTMLSpanElement>(null);
  const [src,setSrc]=useState<string|null>();
  useEffect(()=>{
    let alive=true,url:string|undefined;
    async function load() {
      try {
        const [mime,bytes]=await input.transcriptImage(pane,generation,path);
        if(!alive)return;
        const image=boundedImage(bytes,mime);
        url=URL.createObjectURL(new Blob([image.bytes],{type:mime}));
        setSrc(url);
      } catch { if(alive)setSrc(null); }
    }
    const observer=new IntersectionObserver(entries=>{
      if(alive && entries.some(entry=>entry.isIntersecting)) {
        observer.disconnect();waiting.add(load);drain();
      }
    },{rootMargin:'300px'});
    observer.observe(placeholder.current!);
    return ()=>{alive=false;observer.disconnect();waiting.delete(load);if(url)URL.revokeObjectURL(url);};
  },[pane,generation,path]);
  if(src)return <MarkdownImage src={src} alt={alt} linked={linked}/>;
  return <span ref={placeholder} className={css.alt}>{src===null?'Image preview unavailable · '+(alt||path):alt||path}</span>;
}
export function TranscriptFileImage(props:Props) {
  const session=useOptionalDocumentSession();
  if(!session)return <>{props.alt}</>;
  return <FileImage key={session.view.generation} {...props} pane={session.pane} generation={session.view.generation}/>;
}
