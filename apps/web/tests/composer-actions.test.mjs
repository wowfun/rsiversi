import {test} from "node:test";
import assert from "node:assert/strict";
import {composerKeydown,recallPrompt} from "../composer-actions.js";
const key = extra => ({key:"Enter", preventDefault() {}, ...extra});
const choose = (event, submitKey, composerActions) => composerKeydown(event, {submitKey, composerActions});
test("prompt history leaves composition, menu selection and caret navigation to the editor", () => {
  const editor = {revision:1}, input = {value:"",selectionStart:0,selectionEnd:0};
  const pane = {editor,input,generation:"1",lastTranscript:{blocks:[{key:"old",text:"prompt",recall:"text"}]}};
  const arrow = {key:"ArrowUp",preventDefault() {assert.fail("history consumed a native gesture");}};
  for (const property of ["isComposing","defaultPrevented","ctrlKey","shiftKey","altKey","metaKey"])
    recallPrompt({...arrow,[property]:true},pane);
  recallPrompt({...arrow,keyCode:229},pane);
  recallPrompt(arrow,{...pane,composing:true});
  recallPrompt(arrow,{...pane,composingUntil:Date.now()+1000});
  for(const [start,end] of [[0,0],[0,6]]) {
    recallPrompt(arrow,{...pane,input:{value:"edited",selectionStart:start,selectionEnd:end}});
  }
});
test("keyboard chooses only displayed primary or alternate actions", () => {
  for (const primary of ["queue", "steer"]) {
    const busy = {primary:{id:primary}, alternative:{id:primary === "queue" ? "steer" : "queue"}};
    assert.equal(choose(key(), "enter", busy), "primary");
    for (const modifier of ["ctrlKey", "metaKey"]) {
      assert.equal(choose(key({[modifier]:true}), "enter", busy), "alternative");
      assert.equal(choose(key({[modifier]:true}), "mod_enter", busy), "primary");
      assert.equal(choose(key({[modifier]:true}), "enter", {primary:busy.primary}), "primary");
    }
    assert.equal(choose(key(), "mod_enter", busy), undefined);
    for (const blocked of [{shiftKey:true},{isComposing:true},{keyCode:229},{altKey:true},{key:"a"}]) {
      assert.equal(choose(key({...blocked,ctrlKey:true}), "enter", busy), undefined);
    }
  }
});

test("extra modifiers and held submit gestures preserve both the draft and completion", () => {
  const state = {submitKey:"enter", completionKey() { throw new Error("completion must remain open"); }};
  for (const modifiers of [{altKey:true},{ctrlKey:true,metaKey:true},{ctrlKey:true,shiftKey:true},
    {metaKey:true,shiftKey:true},{getModifierState: key => key === "AltGraph"}]) {
    let prevented = false;
    assert.equal(composerKeydown(key({...modifiers,preventDefault() { prevented = true; }}),state),undefined);
    assert.equal(prevented,true);
  }
  let prevented = false;
  const repeat = key({repeat:true,preventDefault() { prevented = true; }});
  assert.equal(composerKeydown(repeat,{submitKey:"enter"}),undefined);
  assert.equal(prevented,true);
  prevented = false;
  assert.equal(composerKeydown(repeat,{submitKey:"mod_enter"}),undefined);
  assert.equal(prevented,false);
});
