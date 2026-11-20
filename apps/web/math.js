import katex from 'katex';
import 'katex/dist/katex.min.css';
import {copyKatexAsTex} from './vendor/dsh/copy-tex.ts';

if (import.meta.env.PROD) {
  const stylesheet = document.createElement('link');
  stylesheet.rel = 'stylesheet';
  stylesheet.href = '/math.css';
  document.head.append(stylesheet);
}

document.addEventListener('copy', event => {
  const anchor = document.getSelection()?.anchorNode;
  const root = (anchor instanceof Element ? anchor : anchor?.parentElement)?.closest('.markdown');
  if (root?.querySelector('.katex')) copyKatexAsTex(event, root);
});

/** @param {HTMLElement} host @param {string} source @param {boolean} display */
export function typeset(host, source, display) {
  katex.render(source, host, {displayMode:display, trust:false, throwOnError:false, strict:'ignore'});
}
