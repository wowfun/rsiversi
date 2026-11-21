import type {KeyboardEvent, MouseEvent, SyntheticEvent} from 'react';

export function rowContextMenu(show: (event: SyntheticEvent<HTMLElement>, point?: {x: number; y: number}) => void) {
  const plain = (event: KeyboardEvent) => !event.defaultPrevented && !event.nativeEvent.isComposing
    && !event.ctrlKey && !event.altKey && !event.metaKey;
  return {
    onContextMenu(event: MouseEvent<HTMLElement>) {
      show(event, event.clientX || event.clientY ? {x: event.clientX, y: event.clientY} : undefined);
    },
    onKeyDown(event: KeyboardEvent<HTMLElement>) {
      if (!plain(event)) return;
      // On Windows the native menu opens on key release. Keep focus on the opener until then.
      if (event.key === 'ContextMenu') event.preventDefault();
      else if (event.key === 'F10' && event.shiftKey && !event.repeat) show(event);
    },
    onKeyUp(event: KeyboardEvent<HTMLElement>) {
      if (event.key === 'ContextMenu' && plain(event)) show(event);
    },
  };
}
