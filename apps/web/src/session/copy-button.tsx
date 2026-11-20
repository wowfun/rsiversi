import {useEffect, useRef, useState} from 'react';
import {writeClipboard} from '../../mounts.js';
import {report} from '../bridge.ts';
import {IconCheckOutlineRegular, IconCopyOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';

export function CopyButton({text, label, className, size = 16}: {
  text: string | readonly string[]; label: string; className?: string; size?: number;
}) {
  const [copied, setCopied] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const pending = useRef(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy() {
    if (pending.current) return;
    pending.current = true;
    try {
      await writeClipboard(typeof text === 'string' ? text : text.join(''));
      if (!button.current) return;
      setCopied(true);
      timer.current = window.setTimeout(() => {
        pending.current = false;
        setCopied(false);
      }, 1000);
    } catch (error) {
      pending.current = false;
      if (button.current) report(error);
    }
  }

  return <button ref={button} type="button" className={className} aria-label={copied ? 'Copied' : label}
    title={copied ? 'Copied' : label} onClick={() => void copy()}>
    {copied ? <IconCheckOutlineRegular size={size}/> : <IconCopyOutlineRegular size={size}/>}
  </button>;
}
