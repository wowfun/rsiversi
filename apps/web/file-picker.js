// The document holds display state and insertion coordinates. Rust owns every file read.
const node = (tag, text = "", className = "") => {const result = document.createElement(tag); result.textContent = text; result.className = className; return result;};
const pathHex = text => [...new TextEncoder().encode(text)].map(byte=>byte.toString(16).padStart(2,"0")).join("");
export function openFilePicker(pane, call, button) {
  const editor=pane.editor, generation=pane.generation;
  if(!editor || !generation || pane.switching) throw new Error("Open a conversation first");
  pane.fileDialog?.close(); pane.closeCompletions();
  const initialText=editor.text, start=pane.input.selectionStart, end=pane.input.selectionEnd;
  const from=start===end && initialText[start-1]==="@" ? start-1 : start;
  const dialog=node("dialog","","detail-dialog file-picker-dialog"); dialog.setAttribute("aria-label","Insert workspace file path"); pane.fileDialog=dialog;
  const heading=node("div","","dialog-heading"); heading.append(node("h2","Workspace file"),button("Close",()=>dialog.close()));
  const hint=node("p","Browse and preview a file, then insert its path. File contents enter the conversation only when a tool reads them.","hint");
  const path=node("input"); path.placeholder="Workspace-relative path"; path.maxLength=4096; path.setAttribute("aria-label","Workspace-relative file path");
  const controls=node("div","","actions");
  const filter=node("input"); filter.type="search"; filter.placeholder="Filter this directory page"; filter.setAttribute("aria-label","Filter visible files");
  const status=node("p","","hint"); status.setAttribute("role","status");
  const body=node("section","","file-picker-content");
  let current, revision=0, loading=false;
  const alive=()=>dialog.open && pane.fileDialog===dialog && pane.generation===generation && pane.editor===editor;
  const perform=async request=>{
    const mine=++revision; loading=true; controls.querySelectorAll('button').forEach(button=>button.disabled=true); filter.disabled=true; body.inert=true; status.textContent="Reading workspace…";
    try {
      const page=JSON.parse(await call("file_input",JSON.stringify({pane:pane.index,generation,request})));
      if(!alive() || mine!==revision)return;
      current=page; filter.value=""; status.textContent=""; render();
    } catch(error) {if(alive() && mine===revision)status.textContent=`Files unavailable: ${error.message}. Reopen a path to refresh; the draft is unchanged.`;}
    finally {if(alive() && mine===revision){loading=false; controls.querySelectorAll('button').forEach(button=>button.disabled=false); filter.disabled=false;body.inert=false;}}
  };
  const open=(path,file_kind)=>perform({kind:"open",path,file_kind});
  const openText=kind=>{
    if(new TextEncoder().encode(path.value).length>4096) {status.textContent="Path exceeds 4 KiB";return;}
    open(pathHex(path.value==="." ? "" : path.value),kind);
  };
  controls.append(button("List directory",()=>openText("directory")),button("Preview file",()=>openText("file")),button("Workspace root",()=>open("","directory")),button("Reference conversation",()=>{dialog.close();pane.showReferencePicker();}));
  const render=()=>{
    body.replaceChildren(); if(!current)return;
    const page=current, pathBytes=Uint8Array.from(page.path.match(/../g)??[],pair=>parseInt(pair,16));
    let pathText; try {pathText=new TextDecoder("utf-8",{fatal:true}).decode(pathBytes);} catch {pathText=`path_hex:${page.path}`;}
    const name=node("p",pathText||"Workspace root","reference-origin");body.append(name,node("p",`${page.kind==="file" ? "Bytes" : "Entries"} ${page.offset}–${page.next_offset} / ${page.total}`,"hint"));
    if(page.path) {
      let parent=pathBytes.lastIndexOf(47); const parentHex=page.path.slice(0,Math.max(0,parent)*2);
      body.append(button("Parent directory",()=>open(parentHex,"directory")));
    }
    filter.hidden=page.kind!=="directory";
    if(page.kind==="directory") {
      const list=node("div","","file-choice-list");
      for(const entry of page.entries.filter(entry=>entry.name.toLocaleLowerCase().includes(filter.value.toLocaleLowerCase()))) {
        const choice=button(`${entry.name}${entry.kind==="directory" ? "/" : entry.kind ? " · preview" : " · unavailable"}`,()=>open(entry.path,entry.kind)); choice.disabled=!entry.kind;list.append(choice);
      }
      if(!list.children.length)list.append(node("p","No matching files on this page","hint"));
      body.append(list);
    } else {
      body.append(node("pre",page.text));
      const insert=button("Insert file path",()=>{
        if(!alive() || loading)return;
        if(editor.text!==initialText) {status.textContent="Draft changed. Close and reopen the picker at the intended position.";return;}
        try {
          const text=initialText.slice(0,from)+page.locator+initialText.slice(end);
          pane.edit(text,editor.images,editor.references);pane.input.value=text;
          pane.input.setSelectionRange(from+page.locator.length,from+page.locator.length);dialog.close();
        } catch(error) {status.textContent=`Path was not inserted: ${error.message}`;}
      }); insert.className="primary"; body.append(insert);
      body.append(button("View exact hex",()=>perform({kind:"page",revision:page.revision,offset:page.offset,hex:true})),button("View text",()=>perform({kind:"page",revision:page.revision,offset:page.offset,hex:false})));
    }
    if(BigInt(page.offset)>0n)body.append(button("Previous page",()=>perform({kind:"page",revision:page.revision,offset:String(BigInt(page.offset)>BigInt(page.kind==="file" ? 4096 : 16) ? BigInt(page.offset)-BigInt(page.kind==="file" ? 4096 : 16) : 0n),hex:false})));
    if(page.more)body.append(button("Next page",()=>perform({kind:"page",revision:page.revision,offset:page.next_offset,hex:false})));
    body.append(button("Refresh current path",()=>open(page.path,page.kind)));
  };
  filter.addEventListener("input",render);
  dialog.append(heading,hint,path,controls,filter,status,body);
  dialog.addEventListener("close",()=>{
    revision++;
    const request=current && !loading ? {kind:"release",revision:current.revision} : null;
    call("file_input",JSON.stringify({pane:pane.index,generation,request})).catch(()=>{});
    if(pane.fileDialog===dialog)pane.fileDialog=undefined;
    dialog.remove();if(pane.generation===generation && pane.editor===editor)pane.input.focus();
  });
  document.body.append(dialog);dialog.showModal();filter.focus();open("","directory");
}
