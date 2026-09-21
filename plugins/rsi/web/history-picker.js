// Candidates are opened through Rust before a user-selected UTF-8 range can be frozen.
const node=(tag,text="",className="")=>{const n=document.createElement(tag);n.textContent=text;n.className=className;return n;};
export function openHistoryPicker(pane,view,call,button){
  const editor=pane.editor,generation=pane.generation,context=pane.historyContext;
  if(!editor||!generation||!context||pane.switching)throw new Error("Open a native conversation first");
  pane.referenceDialog?.close();
  const dialog=node("dialog","","detail-dialog history-dialog");dialog.setAttribute("aria-label","Search conversation text");pane.referenceDialog=dialog;
  const heading=node("div","","dialog-heading");heading.append(node("h2","Search conversation text"),button("Close",()=>dialog.close(),"quiet"));
  const help=node("p","Search explicit human, assistant and tool text within one conversation. Index progress and omissions are shown below. Open the original before selecting a fragment.","hint");
  const workspace=node("select");workspace.setAttribute("aria-label","History workspace");
  for(const item of view?.catalog?.workspaces??[]){const option=node("option",item.path);option.value=item.id;option.selected=item.path===context.path;workspace.append(option);}
  const kind=node("select");kind.setAttribute("aria-label","History source kind");for(const [id,label]of[["native","Native Session"],["external","External observations"]]){const option=node("option",label);option.value=id;kind.append(option);}
  const source=node("input");source.setAttribute("aria-label","History conversation ID");source.maxLength=256;
  const choices=node("select");choices.setAttribute("aria-label","Saved history source");choices.append(node("option","Choose a saved source…"));
  for(const entry of view?.navigation?.entries??[]){const option=node("option",entry.metadata.title??entry.session);option.value=entry.session;option.dataset.kind="native";choices.append(option);}
  for(const entry of view?.external_catalog?.conversations??[]){const snapshot=entry.snapshot??entry;const option=node("option",`${snapshot.id} · observed`);option.value=snapshot.id;option.dataset.kind="external";choices.append(option);}
  choices.addEventListener("change",()=>{const option=choices.selectedOptions[0];if(option.dataset.kind){source.value=option.value;kind.value=option.dataset.kind;}});
  const query=node("input");query.type="search";query.maxLength=256;query.setAttribute("aria-label","History text query");
  const controls=node("div","","actions"),status=node("p","","hint"),body=node("section","","history-results");status.setAttribute("role","status");
  const scope=()=>({workspace:workspace.value,conversation:{kind:kind.value,id:source.value.trim()}});
  let busy=false,serial=0;
  const alive=()=>dialog.open&&pane.referenceDialog===dialog&&pane.generation===generation&&pane.editor===editor;
  const coverage=value=>`Indexed through ${value.indexed_through}; observed through ${value.observed_through}; omitted ${value.omissions}. ${value.has_more?"More indexing needed.":"Caught up at this observation."}`;
  const perform=async(request,render)=>{if(busy)return;busy=true;const current=++serial;for(const field of [...controls.querySelectorAll("button"),workspace,kind,source,choices,query])field.disabled=true;status.textContent="Reading history owner…";
    try{const reply=JSON.parse(await call("reference_input",JSON.stringify({pane:pane.index,generation,operation:{kind:"history",request}})));if(alive()&&serial===current)render(reply);}catch(error){if(alive())status.textContent=error.message;}finally{busy=false;if(alive())for(const field of [...controls.querySelectorAll("button"),workspace,kind,source,choices,query])field.disabled=false;}
  };
  const search=after=>{const exactScope=scope(),exactQuery=query.value;perform({operation:"search",scope:exactScope,query:exactQuery,after:after??null},reply=>{
    status.textContent=coverage(reply.coverage);body.replaceChildren();
    for(const hit of reply.hits){const item=node("article","","inline-card");item.append(node("p",`${hit.original.record.kind} · record ${hit.original.record.sequence}`,"hint"),node("pre",hit.preview),button("Open original",()=>read(exactScope,hit,0),"quiet"));body.append(item);}
    if(!reply.hits.length)body.append(node("p","No matches in the indexed portion."));
    if(reply.next)body.append(button("More matches",()=>{workspace.value=exactScope.workspace;kind.value=exactScope.conversation.kind;source.value=exactScope.conversation.id;query.value=exactQuery;search(reply.next);},"quiet"));
  });};
  const read=(exactScope,hit,offset)=>perform({operation:"read",scope:exactScope,hit,offset},reply=>{
    status.textContent=`Verified original · record ${hit.original.record.sequence} · bytes ${reply.offset}–${reply.next_offset} of ${hit.original.end}`;body.replaceChildren();
    const original=node("textarea");original.readOnly=true;original.value=reply.text;original.rows=12;original.setAttribute("aria-label","Verified original text");
    const selected=node("p","Select text in the original, then freeze the fragment.","hint");
    const freeze=button("Freeze selected fragment",()=>{
      const start=reply.offset+new TextEncoder().encode(original.value.slice(0,original.selectionStart)).length,end=reply.offset+new TextEncoder().encode(original.value.slice(0,original.selectionEnd)).length;
      if(start===end){status.textContent="Select a nonempty fragment first.";return;}
      perform({operation:"freeze",scope:exactScope,hit,target:context.session,start,end},result=>{
        status.textContent="Frozen. Review these exact bytes before adding to your draft.";body.replaceChildren(node("pre",result.reference.preview),button("Add selected reference to draft",()=>{if(!alive())return;pane.edit(editor.text,editor.images,[...editor.references,result.reference]);dialog.close();},"primary"));
      });
    },"primary");body.append(original,selected,freeze);
    if(reply.offset>0)body.append(button("Previous original page",()=>read(exactScope,hit,Math.max(0,reply.offset-65536)),"quiet"));
    if(reply.has_more)body.append(button("Next original page",()=>read(exactScope,hit,reply.next_offset),"quiet"));
  });
  controls.append(button("Index next batch",()=>perform({operation:"advance",scope:scope()},reply=>{status.textContent=coverage(reply.coverage);body.replaceChildren();}),"quiet"),button("Search text",()=>search(),"primary"),button("Rebuild source index",()=>perform({operation:"rebuild",scope:scope()},reply=>{status.textContent=coverage(reply.coverage);body.replaceChildren();}),"quiet"));
  query.addEventListener("keydown",event=>{if(event.key==="Enter"){event.preventDefault();search();}});
  dialog.append(heading,help,workspace,kind,source,choices,query,controls,status,body);
  dialog.addEventListener("close",()=>{serial++;if(pane.referenceDialog===dialog)pane.referenceDialog=undefined;dialog.remove();if(pane.generation===generation&&pane.editor===editor)pane.input.focus();});document.body.append(dialog);dialog.showModal();source.focus();
}
