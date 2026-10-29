import {test} from 'node:test';
import assert from 'node:assert/strict';
import {updateNotice} from '../notice-state.js';

test('errors and empty Worker notices cannot clear the connection storage warning', () => {
  let state = updateNotice({transient:'',storage:''}, 'storage', 'Draft storage is unavailable');
  state = updateNotice(state, 'transient', 'Request failed');
  assert.deepEqual(state,{transient:'Request failed',storage:'Draft storage is unavailable'});
  assert.equal(updateNotice(state,'transient','Request failed'),state,'same alert keeps snapshot identity');
  state = updateNotice(state,'transient','');
  assert.equal(state.storage,'Draft storage is unavailable');
  assert.deepEqual(updateNotice(state,'storage',''),{transient:'',storage:''});
});
