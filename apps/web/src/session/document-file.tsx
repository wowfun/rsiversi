import {useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode} from 'react';
import {PaperGlyph} from '../../vendor/dsh/dockkit-primitives/FileGlyphs.tsx';
import {FilePanel} from './file-panel.tsx';
import {useOptionalDocumentSession} from '../document-plugins.tsx';
import {input} from '../bridge.ts';
import {workspaceDocument} from './workspace-document.ts';
import css from './markdown-image.module.css';

export function DocumentFileLink({path, line, children, imagesOnly}: {path:string; line?:number|null; children:ReactNode; imagesOnly:boolean}) {
  const session = useOptionalDocumentSession();
  const name = session && workspaceDocument(session.view.workspace.coordinates.path, path);
  if (!name || !session) return <>{children}</>;
  return <button type="button" className={css.link} title={path} onClick={() =>
    window.dispatchEvent(new CustomEvent('rsi:workspace-document', {detail:{
      pane:session.pane, generation:session.view.generation, path:name, line,
    }}))}>
    {!imagesOnly && <PaperGlyph className={css.linkIcon}/>}
    {children}
  </button>;
}

export type DocumentLocation = {path:string; line?:number};

function lineRange(text:string, line:number|undefined) {
  if (!line) return;
  let start = 0;
  for (let current = 1; current < line; current++) {
    const end = text.indexOf('\n', start);
    if (end < 0) return;
    start = end + 1;
  }
  const end = text.indexOf('\n', start);
  return [start, end < 0 ? text.length : end] as const;
}

export function DocumentFilePanel({pane, generation, path, location}: {pane:string; generation:string; path:string; location?:DocumentLocation}) {
  const [text, setText] = useState<string>();
  const [error, setError] = useState('');
  const [revision, reload] = useState(0);
  const mark = useRef<HTMLElement>(null);
  useEffect(() => {
    let active = true;
    setText(undefined);
    setError('');
    void input.transcriptFile(pane, generation, 'document', path).then(([, bytes]) => {
      if (active) setText(new TextDecoder('utf-8', {fatal:true}).decode(bytes));
    }).catch(() => { if (active) setError('Document preview unavailable.'); });
    return () => { active = false; };
  }, [pane, generation, path, revision]);
  const range = useMemo(() => text === undefined ? undefined : lineRange(text, location?.line), [text, location]);
  useLayoutEffect(() => { mark.current?.scrollIntoView({block:'nearest'}); }, [range]);
  return <FilePanel label="Document file" path={path} reload={() => reload(n => n + 1)}>
    {text === undefined ? <p role="status">{error || 'Loading document…'}</p> :
      <pre className={css.documentSource}>{range ? <>{text.slice(0,range[0])}<mark ref={mark}>{text.slice(...range)}</mark>{text.slice(range[1])}</> : text}</pre>}
  </FilePanel>;
}
