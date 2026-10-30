import test from 'node:test';
import assert from 'node:assert/strict';
import {statPosition} from '../stat-position.js';

test('stat panels remain within the viewport at either edge and flip below when necessary',()=>{
  for(const width of [320,390,1440]) {
    const panel={width:Math.min(300,width-24),height:180}, viewport={width,height:600};
    for(const left of [0,width/2,width-20]) {
      for(const top of [0,300,580]) {
        const anchor={left,top,bottom:top+20};
        const position=statPosition(anchor,panel,viewport);
        assert(position.left>=12 && position.left+panel.width<=width-12);
        assert(position.top>=12 && position.top+panel.height<=588);
        if(top===0)assert(position.top>=anchor.bottom);
        if(top===580)assert(position.top+panel.height<anchor.top);
      }
    }
  }
});
