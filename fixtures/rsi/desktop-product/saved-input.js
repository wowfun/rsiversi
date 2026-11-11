const [endpoint, session, text] = arguments;
return import('/drafts.js').then(async ({DraftStore}) => {
  const store = await DraftStore.open(endpoint, {kind: 'local'});
  return (await store.get('main', session))?.text === text;
});
