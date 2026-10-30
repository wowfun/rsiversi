import {createHighlighterCoreSync} from 'shiki/core';
import {createJavaScriptRegexEngine} from 'shiki/engine/javascript';
import light from 'shiki/themes/github-light.mjs';
import dark from 'shiki/themes/github-dark.mjs';
import rust from 'shiki/langs/rust.mjs';
import typescript from 'shiki/langs/typescript.mjs';
import javascript from 'shiki/langs/javascript.mjs';
import python from 'shiki/langs/python.mjs';
import json from 'shiki/langs/json.mjs';
import yaml from 'shiki/langs/yaml.mjs';
import toml from 'shiki/langs/toml.mjs';
import shell from 'shiki/langs/shellscript.mjs';
import html from 'shiki/langs/html.mjs';
import css from 'shiki/langs/css.mjs';

const aliases={rs:'rust',ts:'typescript',tsx:'typescript',js:'javascript',jsx:'javascript',py:'python',yml:'yaml',sh:'shellscript',bash:'shellscript',htm:'html'};
const supported=new Set(['rust','typescript','javascript','python','json','yaml','toml','shellscript','html','css']);
let highlighter;
const incomplete = new Error('Highlight tokenization did not finish');
const timedOut = new Error('Highlight tokenization exhausted its time budget');
function completeTokens(text,lang,budgetMs,onBudgetExceeded) {
  const grammar=highlighter.getLanguage(lang),own=Object.hasOwn(grammar,'tokenizeLine2'),tokenize=grammar.tokenizeLine2;
  // This grammar belongs only to this synchronous highlighter. Shiki 4.3.1
  // otherwise discards stoppedEarly and combines partial state across themes.
  const started=Date.now();
  let tokenCount=0;
  grammar.tokenizeLine2=function(...args){
    if(Date.now()-started>=budgetMs)throw timedOut;
    const result=tokenize.apply(this,args);
    // TextMate's binary tokens are offset/metadata pairs. Bound allocation
    // before Shiki expands all lines and combines the two theme passes.
    tokenCount+=result.tokens.length/2;
    if(result.stoppedEarly||Date.now()-started>=budgetMs)throw timedOut;
    if(tokenCount>32768)throw incomplete;
    return result;
  };
  try {return highlighter.codeToTokens(text,{lang,themes:{light:'github-light',dark:'github-dark'},tokenizeTimeLimit:budgetMs}).tokens}
  catch(error){if(error===timedOut){onBudgetExceeded();return null}if(error===incomplete)return null;throw error}
  finally {if(own)grammar.tokenizeLine2=tokenize;else delete grammar.tokenizeLine2}
}
export function highlightCode(text,language,{budgetMs=500,onBudgetExceeded=()=>{}}={}) {
  const lang=aliases[language]??language;
  if(!supported.has(lang)||text.length>256*1024||new TextEncoder().encode(text).length>256*1024||text.split('\n').some(line=>line.length>4096))return null;
  highlighter??=createHighlighterCoreSync({themes:[light,dark],langs:[rust,typescript,javascript,python,json,yaml,toml,shell,html,css],engine:createJavaScriptRegexEngine()});
  const tokens=completeTokens(text,lang,budgetMs,onBudgetExceeded);
  if(!tokens)return null;
  if(tokens.reduce((count,line)=>count+line.length,0)>16384)return null;
  return tokens.map(line=>line.map(token=>({content:token.content,light:token.htmlStyle?.color,dark:token.htmlStyle?.['--shiki-dark']})));
}
export function colorToken(element,token) {
  for(const theme of ['light','dark'])if(/^#[0-9a-f]{6,8}$/i.test(token[theme]??''))element.style.setProperty(`--code-${theme}`,token[theme]);
  element.classList.add('code-token');
}
