import {test} from "node:test";
import assert from "node:assert/strict";
import {keyboardAction} from "../composer-actions.js";
const key = extra => ({key:"Enter", ...extra});
test("keyboard chooses only displayed primary or alternate actions", () => {
  for (const primary of ["queue", "steer"]) {
    const busy = {primary:{id:primary}, alternative:{id:primary === "queue" ? "steer" : "queue"}};
    assert.equal(keyboardAction(key(), "enter", busy), "primary");
    for (const modifier of ["ctrlKey", "metaKey"]) {
      assert.equal(keyboardAction(key({[modifier]:true}), "enter", busy), "alternative");
      assert.equal(keyboardAction(key({[modifier]:true}), "mod_enter", busy), "primary");
      assert.equal(keyboardAction(key({[modifier]:true}), "enter", {primary:busy.primary}), "primary");
    }
    assert.equal(keyboardAction(key(), "mod_enter", busy), undefined);
    for (const blocked of [{shiftKey:true},{isComposing:true},{keyCode:229},{altKey:true},{key:"a"}]) {
      assert.equal(keyboardAction(key({...blocked,ctrlKey:true}), "enter", busy), undefined);
    }
  }
});
