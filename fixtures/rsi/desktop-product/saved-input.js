const [session, text] = arguments;
return import('/drafts.js').then(async ({DraftStore}) => {
  if (!window.fixtureSavedInput) {
    const state = {session, store: undefined, original: DraftStore.prototype.edit};
    DraftStore.prototype.edit = function(record, ...args) {
      if (JSON.stringify(record.key) === JSON.stringify(this.key('main', state.session))) state.store ??= this;
      return state.original.call(this, record, ...args);
    };
    window.fixtureSavedInput = state;
  }
  const state = window.fixtureSavedInput;
  if (state.session !== session) return false;
  if (text === undefined) return true;
  if ((await state.store?.get('main', session))?.text !== text) return false;
  DraftStore.prototype.edit = state.original;
  return true;
});
