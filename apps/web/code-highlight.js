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
const languages={rust,typescript,javascript,python,json,yaml,toml,shellscript:shell,html,css};
let highlighter;
const incomplete = new Error('Incomplete optional highlighting');
const warmed = new Set();
const themes = {light:'github-light',dark:'github-dark'};
function warm(lang) {
  highlighter??=createHighlighterCoreSync({themes:[light,dark],langs:[],engine:createJavaScriptRegexEngine()});
  if (!warmed.has(lang)) {
    highlighter.loadLanguageSync(languages[lang]);
    highlighter.codeToTokens('const value = "warm"; # comment',{lang,themes});
    warmed.add(lang);
  }
}
/** @typedef {{ content: string, light?: string, dark?: string }} HighlightToken */
export function createHighlightSession(text,language) {
  const lang=aliases[language]??language;
  if(!Object.hasOwn(languages,lang)||text.length>256*1024||new TextEncoder().encode(text).length>256*1024||/\r(?!\n)/.test(text))return null;
  const lines=text.split('\n');
  if(lines.some(line=>line.length>4096))return null;
  let cursor=0, state, rawCount=0, outputCount=0, ready=false;
  /** @type {HighlightToken[][]} */
  const completed=[];
  return {
    get retainedTokens(){return rawCount},
    get progress(){return cursor+(ready?1:0)},
    step(budgetMs) {
      const started=Date.now();
      if(!ready){warm(lang);ready=true;if(Date.now()-started>=budgetMs)return {status:'pending'};}
      const grammar=highlighter.getLanguage(lang),own=Object.hasOwn(grammar,'tokenizeLine2'),tokenize=grammar.tokenizeLine2;
      let lineStarted;
      // Shiki discards TextMate stoppedEarly. Never retain a partial line/state.
      grammar.tokenizeLine2=function(...args){
        if(Date.now()-lineStarted>=budgetMs)throw incomplete;
        const result=tokenize.apply(this,args);
        rawCount+=result.tokens.length/2;
        if(result.stoppedEarly||Date.now()-lineStarted>=budgetMs||rawCount>32768)throw incomplete;
        return result;
      };
      try {
        while(cursor<lines.length) {
          if(Date.now()-started>=budgetMs)return {status:'pending'};
          const line=lines[cursor], cr=line.endsWith('\r');
          lineStarted=Date.now();
          const result=highlighter.codeToTokens(cr?line.slice(0,-1):line,{lang,themes,grammarState:state,tokenizeTimeLimit:budgetMs});
          const row=result.tokens[0].map(token=>({content:token.content,light:token.htmlStyle?.color,dark:token.htmlStyle?.['--shiki-dark']}));
          if(cr)row.push({content:'\r',light:undefined,dark:undefined});
          outputCount+=row.length;
          if(outputCount>16384)return {status:'plain'};
          state=highlighter.getLastGrammarState(result.tokens);
          completed.push(row);cursor++;
        }
        return {status:'complete',tokens:completed};
      } catch(error) {if(error===incomplete)return {status:'plain'};throw error;}
      finally {if(own)grammar.tokenizeLine2=tokenize;else delete grammar.tokenizeLine2;}
    },
  };
}
export function colorToken(element,token) {
  for(const theme of ['light','dark'])if(/^#[0-9a-f]{6,8}$/i.test(token[theme]??''))element.style.setProperty(`--code-${theme}`,token[theme]);
  element.classList.add('code-token');
}
