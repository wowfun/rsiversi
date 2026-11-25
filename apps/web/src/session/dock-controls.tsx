import type {ReactNode} from 'react';
import {Tooltip} from '../../vendor/dsh/dockkit-primitives/Tooltip.tsx';
import css from './composer-dock.module.css';

export function DockButton({label, disabled=false, onClick, children}: {
  label:string; disabled?:boolean; onClick():void; children:ReactNode;
}) {
  return <Tooltip label={label} portal side="bottom" delayMs={500} disabled={disabled}>
    <button type="button" aria-label={label} disabled={disabled} onClick={onClick}>{children}</button>
  </Tooltip>;
}

export function InlineEditor({label, value, pending, onChange, onSave, onCancel}: {
  label:string; value:string; pending:boolean; onChange(value:string):void; onSave():void; onCancel():void;
}) {
  return <textarea className={css.editor} autoFocus rows={1} aria-label={label} value={value} readOnly={pending}
    onChange={event=>onChange(event.currentTarget.value)} onKeyDown={event=>{
      if (pending || event.nativeEvent.isComposing || event.nativeEvent.keyCode===229) return;
      if (event.key==='Enter' && !event.shiftKey) {
        event.preventDefault();
        onSave();
      } else if (event.key==='Escape') {
        event.preventDefault();
        onCancel();
      }
    }}/>;
}
