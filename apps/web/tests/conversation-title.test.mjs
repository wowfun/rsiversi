import {test} from 'node:test';
import assert from 'node:assert/strict';
import {conversationTitle} from '../conversation-title.js';
test('each destination uses its own surface identity, independent of the selected pane',()=>{
  const view={surfaces:{main:{kind:'native',session:'main-id',workspace:'w'},compare:{kind:'native',session:'compare-id',workspace:'w'}},
    catalog:{workspaces:[]},navigation:{entries:[{session:'main-id',metadata:{title:'Main session'}},{session:'compare-id',metadata:{title:'Compare session'}}]}};
  for(const selected of ['main','compare','main']) {
    view.selected=selected;
    assert.equal(conversationTitle(view,'main'),'Main session');assert.equal(conversationTitle(view,'compare'),'Compare session');
  }
});
test('a native conversation without title or path retains a nonempty tab label',()=>{
  const view={surfaces:{main:{kind:'native',session:'empty',path:'',workspace:null}},catalog:{workspaces:[]}};
  assert.equal(conversationTitle(view,'main'),'New conversation');
});
test('the current navigation entry remains authoritative over an older group title',()=>{
  const view={surfaces:{main:{kind:'native',session:'same',path:'/workspace',workspace:null}},catalog:{workspaces:[]},
    navigation:{entries:[{session:'same',metadata:{title:null}}],groups:{old:{entries:[{session:'same',metadata:{title:'Old title'}}]}}}};
  assert.equal(conversationTitle(view,'main'),'workspace');
});
