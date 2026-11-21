import type {ReactNode} from 'react';
import {InlineHoverCard} from '../../vendor/dsh/dockkit-primitives/InlineHoverCard.tsx';
import {PhotoGlyph} from '../../vendor/dsh/dockkit-primitives/PhotoGlyph.tsx';
import {useOptionalDocumentSession} from '../document-plugins.tsx';
import {FileImage} from './transcript-file-image.tsx';
import css from './markdown-image.module.css';

export function ImageFileLink({path, imagesOnly, children}: {path:string; imagesOnly:boolean; children:ReactNode}) {
  const session = useOptionalDocumentSession();
  if (!session) return <>{children}</>;
  const {pane, view: {generation}} = session;
  const anchor = <button type="button" className={css.fileLink} title={imagesOnly ? path : undefined}
    onClick={() => window.dispatchEvent(new CustomEvent('rsi:file-image', {detail: {pane, generation, path}}))}>
    {!imagesOnly && <PhotoGlyph className={css.linkIcon}/>}
    {children}
  </button>;
  return imagesOnly ? anchor : <InlineHoverCard anchor={anchor}>
    <span className={css.previewFrame}><FileImage pane={pane} generation={generation} path={path} alt={path} linked/></span>
    <span className={css.previewName}>{path.split(/[\\/]/).at(-1)}</span>
  </InlineHoverCard>;
}
