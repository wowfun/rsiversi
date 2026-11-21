import type {ReactNode, Ref} from 'react';
import {IconRefreshOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './markdown-image.module.css';

export function FilePanel({label,path,reload,scrollRef,children}: {label:string; path:string; reload():void; scrollRef?:Ref<HTMLDivElement>; children:ReactNode}) {
  return <section className={css.panel} aria-label={label}>
    <header className={css.fileHeader}>
      <span title={path}>{path}</span>
      <button type="button" aria-label="Reload file" title="Reload file" onClick={reload}>
        <IconRefreshOutlineRegular size={16}/>
      </button>
    </header>
    <div ref={scrollRef} className={css.fileScroll}>{children}</div>
  </section>;
}
