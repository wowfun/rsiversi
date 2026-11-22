import {useRef, type ReactNode, type RefObject} from 'react';
import {useTextInteraction, type TextInteractionGesture} from '../../vendor/dsh/quote/text-interaction.ts';
import button from '../../vendor/dsh/quote/QuoteSelection.module.css';
import {run} from '../bridge.ts';
import css from './chat.module.css';

const actions = ['quote'];
const select = (gesture: TextInteractionGesture) =>
  gesture.trigger === 'selection' && gesture.hasSelection ? 'quote' : null;

function QuoteAction({root, insert}: {
  root: RefObject<HTMLElement>; insert: (text: string) => boolean;
}) {
  const surface = useRef<HTMLDivElement>(null);
  const {active, dismiss, readText} = useTextInteraction(root, surface, select, actions);
  return <div ref={surface} className={css.quoteAction} data-text-interaction-exclude="">
    {active && <button type="button" className={button.action}
      onPointerDown={event => event.preventDefault()}
      onClick={() => void run(() => {
        const text = readText();
        if (text === null) { dismiss(); return; }
        if (insert(text)) dismiss();
      })}>Quote text</button>}
  </div>;
}

export function TranscriptFlow({children, insert}: {children: ReactNode; insert?: (text: string) => boolean}) {
  const root = useRef<HTMLDivElement>(null);
  return <div className="transcript-flow" ref={root}>
    {children}
    {insert && <QuoteAction root={root} insert={insert}/>}
  </div>;
}
