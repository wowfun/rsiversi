import {useEffect, useRef, useState} from 'react';
import {input} from '../bridge.ts';
import {useOptionalDocumentSession} from '../document-plugins.tsx';
import {boundedImage} from '../../image-bounds.js';
import {releaseTranscriptImage} from './image-intake.ts';
import {MarkdownImage} from './markdown-image.tsx';
import {IconRefreshOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './markdown-image.module.css';

type Props = {path: string; alt: string; linked: boolean};
interface Preview { url: string; original: File; unavailable(): void }

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

export function FileImage({pane, generation, path, alt, linked, lazy = false}: Props & {pane: string; generation: string; lazy?: boolean}) {
  const placeholder = useRef<HTMLSpanElement>(null);
  const [preview, setPreview] = useState<Preview | null>();

  useEffect(() => {
    let mounted = true;
    let loaded: Preview | undefined;
    function release() {
      if (loaded) {
        URL.revokeObjectURL(loaded.url);
        releaseTranscriptImage(loaded.original);
        loaded = undefined;
      }
    }
    function unavailable() {
      release();
      if (mounted) setPreview(null);
    }
    async function load() {
      try {
        const [mime, bytes] = await input.transcriptImage(pane, generation, path);
        if (!mounted) return;
        const image = boundedImage(bytes, mime);
        const name = path.split(/[\\/]/).at(-1) || 'image';
        const original = new File([bytes], name, {type: mime});
        // SVG previews normalize dimensions. A drag still carries the original file.
        const display = image.bytes === bytes ? original : new Blob([image.bytes], {type: mime});
        loaded = {url: URL.createObjectURL(display), original, unavailable};
        setPreview(loaded);
      } catch {
        unavailable();
      }
    }
    function schedule() { waiting.add(load); drain(); }
    let observer: IntersectionObserver | undefined;
    if (lazy) {
      observer = new IntersectionObserver(entries => {
        if (mounted && entries.some(entry => entry.isIntersecting)) {
          observer?.disconnect();
          schedule();
        }
      }, {rootMargin: '300px'});
      observer.observe(placeholder.current!);
    } else schedule();
    return () => {
      mounted = false;
      observer?.disconnect();
      waiting.delete(load);
      release();
    };
  }, [pane, generation, path, lazy]);

  if (preview) return <MarkdownImage src={preview.url} original={preview.original} alt={alt} linked={linked}
    onDecodeError={preview.unavailable}/>;
  const label = alt || path;
  return <span ref={placeholder} className={css.alt}>{preview === null ? 'Image preview unavailable · ' + label : label}</span>;
}

export function TranscriptFileImage(props: Props) {
  const session = useOptionalDocumentSession();
  if (!session) return <>{props.alt}</>;
  return <FileImage key={session.view.generation} {...props} pane={session.pane} generation={session.view.generation} lazy/>;
}

export function FileImagePanel({pane, generation, path}: {pane:string; generation:string; path:string}) {
  const [revision, reload] = useState(0);
  return <section className={css.panel} aria-label="Image file">
    <header className={css.fileHeader}>
      <span title={path}>{path}</span>
      <button type="button" aria-label="Reload file" title="Reload file" onClick={() => reload(n => n + 1)}>
        <IconRefreshOutlineRegular size={16}/>
      </button>
    </header>
    <div className={css.fileScroll}><div className={css.fileFrame} data-image-preview="">
      <FileImage key={revision} pane={pane} generation={generation} path={path} alt={path} linked/>
    </div></div>
  </section>;
}
