import {Fragment, useLayoutEffect, useRef, useState} from 'react';
import {IconPlusOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {run} from '../bridge.ts';
import type {ComposerOwner, ComposerProjection} from './composer-state.ts';
import css from './composer-menu.module.css';

interface Props {
  owner: ComposerOwner;
  data?: ComposerProjection | null;
  empty: boolean;
  id: string;
  choice: number | null;
  choose(value: number | null): void;
}
const label = 'Add files or run commands';

export function ComposerAdd({owner, data, empty, id, choice, choose}: Props) {
  const open = choice !== null;
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [loading, setLoading] = useState(false);

  const loadCommands = async () => {
    setLoading(true);
    await run(async () => {
      if (!await owner.prepareComposer()) return;
      owner.captureCommandSelection();
      await owner.action('commands');
    });
    setLoading(false);
  };
  useLayoutEffect(() => {
    if (!open) return;
    const list = menu.current!;
    const editor = owner.input;
    const form = editor.form!;
    const fit = () => {
      list.style.maxHeight = `${Math.max(0, Math.min(400, form.getBoundingClientRect().top - 12))}px`;
    };
    const close = () => choose(null);
    const outside = (event: Event) => {
      if (event.target instanceof Node && !form.contains(event.target)) close();
    };
    const focus = (event: FocusEvent) => {
      if (event.target !== editor && event.target !== trigger.current && event.target !== list) close();
    };
    const key = (event: KeyboardEvent) => {
      if (event.isComposing || event.keyCode === 229 || event.ctrlKey || event.metaKey || event.altKey) return;
      const enabled = Array.from(list.querySelectorAll<HTMLButtonElement>('[role=option]:enabled'));
      const active = enabled.findIndex(row => row.getAttribute('aria-selected') === 'true');
      if (event.key === 'Escape' || (event.key === 'Tab' && event.shiftKey)) close();
      else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
        if (!enabled.length) return;
        const dir = event.key === 'ArrowDown' ? 1 : -1;
        const next = active < 0 && dir < 0 ? enabled.length - 1 : (active + dir + enabled.length) % enabled.length;
        choose(Number(enabled[next].value));
        enabled[next].scrollIntoView({block:'nearest'});
      } else if ((event.key === 'Enter' || event.key === 'Tab') && !event.shiftKey) {
        enabled[active]?.click();
      } else return;
      event.preventDefault();
      event.stopPropagation();
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(form);
    const events = new AbortController();
    const {signal} = events;
    form.addEventListener('keydown', key, {capture:true, signal});
    editor.addEventListener('input', close, {signal});
    editor.addEventListener('compositionstart', close, {signal});
    document.addEventListener('pointerdown', outside, {capture:true, signal});
    document.addEventListener('focusin', focus, {signal});
    window.addEventListener('resize', fit, {signal});
    if (editor.disabled) list.focus();
    return () => { observer.disconnect(); events.abort(); };
  }, [open, owner, choose]);

  const toggle = () => {
    if (open) { choose(null); owner.focusInput(); return; }
    owner.focusInput();
    owner.closeCompletions();
    owner.captureCommandSelection();
    choose(0);
    if (data && !data.commands && !loading) void loadCommands();
  };
  const launcher = <button ref={trigger} type="button" className={`${css.trigger} ${css.plus}`}
    aria-label={label} aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? id : undefined}
    onMouseDown={event => event.preventDefault()} onClick={toggle}><IconPlusOutlineRegular/></button>;
  if (!open) return launcher;

  const state = owner.composerView;
  const bound = (action: () => unknown) => run(async () => {
    if (await owner.prepareComposer()) await action();
  });
  const unavailable = (disabled?: boolean) => data ? disabled : !state || state.disabled;
  type Row = {label:string; section:string; description?:string; disabled?:boolean; keepOpen?:boolean; pick():unknown};
  const rows: Row[] = [
    {label:'File', section:'Add', disabled:unavailable(state?.fileDisabled), pick:() => bound(() => owner.showFilePicker())},
    ...(state?.mediaLimits ? [{label:'Images', section:'Add', disabled:state.imageDisabled, pick:() => owner.imageInput.click()}] : []),
    {label:'Conversation', section:'Add', disabled:unavailable(state?.referenceDisabled), pick:() => bound(() => owner.showReferencePicker())},
    {label:'Search history', section:'Add', disabled:unavailable(state?.referenceDisabled), pick:() => bound(() => owner.showHistoryPicker())},
  ];
  if (empty) rows.push({label:state?.exporting ? 'Cancel export' : 'Export', section:'Add',
    disabled:unavailable(false), pick:() => bound(() => owner.exportSession(''))});
  if (data?.commands) {
    for (const command of data.commands.commands) {
      if (command.composer_input) rows.push({label:`/${command.name}`, section:'Commands', description:command.description,
        disabled:data.commands.revision.kind === 'draft' && !command.draft_safe, pick:() => run(() => owner.selectCommand(command))});
    }
  } else {
    rows.push({label:loading ? 'Loading commands…' : 'Commands', section:'Add', keepOpen:true,
      disabled:loading || unavailable(false), pick:loadCommands});
  }
  return <>{launcher}
    <div ref={menu} id={id} className={css.addList} role="listbox" aria-label={label}
      aria-activedescendant={choice !== null && rows[choice] ? `${id}-${choice}` : undefined} tabIndex={-1}>
      {rows.map((row, index) => <Fragment key={row.label}>
        {rows[index - 1]?.section !== row.section && <div className={css.heading} role="presentation">{row.section}</div>}
        <button type="button" role="option" className={css.option} id={`${id}-${index}`} value={index}
          tabIndex={-1} aria-label={row.label} aria-selected={choice === index} disabled={row.disabled}
          onMouseMove={() => { if (choice !== index) choose(index); }}
          onMouseDown={event => event.preventDefault()}
          onClick={() => { if (!row.keepOpen) choose(null); void row.pick(); }}>
          {row.label}{row.description && <span className={css.description}>{row.description}</span>}
        </button>
      </Fragment>)}
    </div>
  </>;
}
