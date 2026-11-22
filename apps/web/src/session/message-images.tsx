import {useEffect, useRef, useState} from 'react';
import type {ImageInterest} from '../details/model.ts';
import {singleFit} from '../../vendor/dsh/attachment/single-fit.ts';
import css from '../../vendor/dsh/attachment/MessageImage.module.css';
import type {Media} from './composer-state.ts';
import {startTranscriptImageDrag} from './image-intake.ts';
import {ImageLightbox} from './markdown-image.tsx';
import type {TranscriptOwner} from './transcript.tsx';

type Source = {owner:TranscriptOwner; generation:string; messageKey:string};

function Thumbnail({owner, generation, messageKey, media, index, single, visible}: Source & {
  media:Media; index:number; single:boolean; visible:boolean;
}) {
  const [attempt, retry] = useState(0);
  const [original, setOriginal] = useState<string>();
  const originalRead = useRef<ImageInterest>();
  const [preview, setPreview] = useState<{url:string; interest:ImageInterest} | null>();
  const dragged = useRef(false);
  const fit = single ? singleFit(media) : undefined;
  const label = 'Image ' + (index + 1);
  const acquire = () => owner.acquireTranscriptImage(media, generation, messageKey, index);
  useEffect(() => {
    setPreview(undefined);
    if (!visible) return;
    let live = true;
    let interest: ImageInterest;
    try { interest = acquire(); }
    catch { setPreview(null); return; }
    void interest.result.then(url => {
      if (live) setPreview(url ? {url, interest} : null);
    }, () => { if (live) setPreview(null); });
    return () => { live = false; interest.release(); };
  }, [visible, attempt, owner, generation, messageKey, index, media.id]);
  useEffect(() => () => {
    originalRead.current?.release();
    originalRead.current = undefined;
  }, []);

  async function openOriginal() {
    if (originalRead.current) return;
    try {
      const interest = acquire();
      originalRead.current = interest;
      const url = await interest.result;
      if (originalRead.current !== interest) return;
      if (!url) throw new Error('This image is no longer available');
      setOriginal(url);
    } catch {
      originalRead.current?.release();
      originalRead.current = undefined;
      setPreview(null);
    }
  }
  function closeOriginal() {
    originalRead.current?.release();
    originalRead.current = undefined;
    setOriginal(undefined);
  }

  async function dragFile() {
    const interest = acquire();
    try {
      await interest.result;
      if (!interest.file) throw new Error('This image is no longer available');
      return interest.file;
    } finally { interest.release(); }
  }

  const variant = single ? 'single' : 'tile';
  return <>
    <button type="button" className={preview === null ? css.error : css.frame} data-variant={variant}
      style={fit && {width:fit.width,height:fit.height}}
      aria-label={preview === null ? 'Retry loading '+label : 'Open '+label}
      aria-busy={visible && preview === undefined || undefined}
      onClick={event => {
        if (preview === null) retry(n => n + 1);
        else if (preview && !(event.detail && dragged.current)) void openOriginal();
      }}>
      {preview === null ? 'Could not load image. Retry' : preview
        ? <img alt={label} src={preview.url} style={{objectPosition:fit?.objectPosition}} draggable
            onLoad={() => preview.interest.release()}
            onError={() => { preview.interest.release(); setPreview(null); }}
            onPointerDown={() => { dragged.current = false; }}
            onDragStart={event => { dragged.current = true; startTranscriptImageDrag(event.dataTransfer, dragFile); }}/>
        : <span className={css.loading}>{visible && 'Loading…'}</span>}
    </button>
    {original && <ImageLightbox src={original} alt={label} close={closeOriginal}/>}
  </>;
}

export function MessageImages({owner,generation,messageKey,images}: Source & {images:Media[]}) {
  const gallery = useRef<HTMLDivElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    // A resize can deliver both exit and re-entry for this gallery in the same batch.
    const observer = new IntersectionObserver(entries => setVisible(entries.at(-1)!.isIntersecting), {rootMargin:'160px'});
    observer.observe(gallery.current!);
    return () => observer.disconnect();
  }, []);
  return <div ref={gallery} className={css.gallery} data-align="end" data-message-attachments="" aria-label="Message images">
    {images.map((media,index) => <Thumbnail key={media.id + ':' + index} {...{owner,generation,messageKey,media,index,visible}} single={images.length === 1}/>)}
  </div>;
}
