import {useEffect, useRef, useState} from 'react';
import {input} from '../bridge.ts';
import {useOptionalDocumentSession} from '../document-plugins.tsx';
import {boundedImage} from '../../image-bounds.js';
import {releaseTranscriptImage} from './image-intake.ts';
import {MarkdownImage} from './markdown-image.tsx';
import css from './markdown-image.module.css';

type Props = {path: string; alt: string; linked: boolean};
interface Preview { url: string; original: File }

const waiting = new Set<() => Promise<void>>();
let active = 0;

function drain() {
  while (active < 2 && waiting.size) {
    const next = waiting.values().next().value!;
    waiting.delete(next);
    active++;
    void next().finally(() => {
      active--;
      drain();
    });
  }
}

function FileImage({pane, generation, path, alt, linked}: Props & {pane: string; generation: string}) {
  const placeholder = useRef<HTMLSpanElement>(null);
  const [preview, setPreview] = useState<Preview | null>();

  useEffect(() => {
    let mounted = true;
    let loaded: Preview | undefined;
    async function load() {
      try {
        const [mime, bytes] = await input.transcriptImage(pane, generation, path);
        if (!mounted) return;
        const image = boundedImage(bytes, mime);
        const name = path.split(/[\\/]/).at(-1) || 'image';
        const original = new File([bytes], name, {type: mime});
        // SVG previews normalize dimensions. A drag still carries the original file.
        const display = image.bytes === bytes ? original : new Blob([image.bytes], {type: mime});
        loaded = {url: URL.createObjectURL(display), original};
        setPreview(loaded);
      } catch {
        if (mounted) setPreview(null);
      }
    }
    const observer = new IntersectionObserver(entries => {
      if (mounted && entries.some(entry => entry.isIntersecting)) {
        observer.disconnect();
        waiting.add(load);
        drain();
      }
    }, {rootMargin: '300px'});
    observer.observe(placeholder.current!);
    return () => {
      mounted = false;
      observer.disconnect();
      waiting.delete(load);
      if (loaded) {
        URL.revokeObjectURL(loaded.url);
        releaseTranscriptImage(loaded.original);
      }
    };
  }, [pane, generation, path]);

  if (preview) return <MarkdownImage src={preview.url} original={preview.original} alt={alt} linked={linked}/>;
  const label = alt || path;
  return <span ref={placeholder} className={css.alt}>{preview === null ? 'Image preview unavailable · ' + label : label}</span>;
}

export function TranscriptFileImage(props: Props) {
  const session = useOptionalDocumentSession();
  if (!session) return <>{props.alt}</>;
  return <FileImage key={session.view.generation} {...props} pane={session.pane} generation={session.view.generation}/>;
}
