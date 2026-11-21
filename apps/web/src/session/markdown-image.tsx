import {useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {Modal} from '../presentation.tsx';
import {IconCloseOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './markdown-image.module.css';
import {startTranscriptImageDrag} from './image-intake.ts';

export function MarkdownImage({src,alt,linked,original,onDecodeError}: {
  src:string; alt:string; linked:boolean; original?:File; onDecodeError?:()=>void
}) {
  const [failed,setFailed]=useState(false);
  const [open,setOpen]=useState(false);
  const dragged = useRef(false);
  if (failed) return <span className={css.alt}>Image preview unavailable · {alt || src}</span>;
  const image=<img className={css.image} alt={alt} loading={original ? 'eager' : 'lazy'} decoding="async"
    {...(original && !linked && {
      draggable: true,
      onPointerDown: () => { dragged.current = false; },
      onDragStart: (event: React.DragEvent<HTMLImageElement>) => {
        startTranscriptImageDrag(event.dataTransfer, original);
        dragged.current = true;
      },
      onClickCapture: (event: React.MouseEvent<HTMLImageElement>) => {
        if (dragged.current && event.detail) { event.preventDefault(); event.stopPropagation(); }
      },
    })}
    referrerPolicy="no-referrer" src={src} onError={onDecodeError ?? (()=>setFailed(true))}/>;
  if (linked) return image;
  return <>
    <button type="button" className={css.thumbnail} aria-label={alt ? 'View full image: '+alt : 'View full image'}
      onClick={()=>setOpen(true)}>{image}</button>
    {open && createPortal(<Modal className={css.lightbox} label="Image preview" close={()=>setOpen(false)} initialFocus="button">
      <div className={css.mask} aria-hidden="true" onMouseDown={()=>setOpen(false)}/>
      <img className={css.original} alt={alt} referrerPolicy="no-referrer" src={src}/>
      <button type="button" className={css.close} aria-label="Close image preview" onClick={()=>setOpen(false)}><IconCloseOutlineRegular size={16}/></button>
    </Modal>,document.body)}
  </>;
}
