// Display-only external conversations; Rust owns observation and one-shot controls.
export function externalPaneClass({ element, button, command, perform }) {
  const drafts = new Map();
  function save(id, text) {
    if (!id) return;
    drafts.delete(id); drafts.set(id, text);
    while (drafts.size > 16 || [...drafts.values()].reduce((bytes, value) => bytes + new TextEncoder().encode(value).length, 0) > 1024 * 1024) drafts.delete(drafts.keys().next().value);
  }
  return class ExternalPane {
    static clearDrafts() { drafts.clear(); }
    kind = "external";
    blocks = new Map();
    constructor(index) {
      this.index = index;
      this.node = element("section", "pane external-pane");
      this.node.setAttribute("aria-label", "External conversation");
      this.name = element("h2", "pane-name");
      this.path = element("p", "pane-session");
      this.status = element("p", "external-status"); this.status.setAttribute("role", "status");
      const header = element("header", "pane-header"); header.append(this.name, this.path, this.status);
      this.controls = element("div", "external-controls");
      this.refresh = button("Refresh", () => this.control({kind:"refresh"}), "quiet");
      this.beginning = button("History from beginning", () => this.control({kind:"beginning"}), "quiet");
      this.next = button("Next history page", () => this.control({kind:"next"}), "quiet");
      this.live = button("Follow latest", () => this.control({kind:"live"}), "quiet");
      this.resume = button("Resume connection", () => this.control({kind:"reconnect",setup:"resume"}), "quiet");
      this.load = button("Reload remote history", () => this.control({kind:"reconnect",setup:"load"}), "quiet");
      this.close = button("Close peer", () => this.control({kind:"close"}), "quiet");
      this.controls.append(this.refresh,this.beginning,this.next,this.live,this.resume,this.load,this.close);
      this.permissions = element("div", "external-permissions");this.permissions.setAttribute("aria-label","External permissions");
      this.transcript = element("div", "transcript external-transcript");this.transcript.setAttribute("aria-label","Observed history");
      this.source = element("section", "external-source");this.source.hidden=true;
      this.notice = element("p", "settings-error");this.notice.setAttribute("role","alert");
      this.composer = element("form", "composer external-composer");
      this.input = element("textarea");this.input.rows=3;this.input.maxLength=131072;this.input.placeholder="Send a message to this external agent";this.input.setAttribute("aria-label","External message");
      this.input.addEventListener("input",()=>{this.uncertain=false;save(this.id,this.input.value);this.renderComposer();});
      this.input.addEventListener("keydown",event=>{if(event.isComposing)return;if(event.key==="Enter" && !event.shiftKey && ((event.ctrlKey||event.metaKey)||this.enterSubmit)){event.preventDefault();perform(()=>this.submit());}});
      this.composer.addEventListener("submit",event=>{event.preventDefault();perform(()=>this.submit());});
      this.send=button("Send ↗",()=>this.submit(),"primary");
      this.cancel=button("Cancel prompt",()=>this.control({kind:"cancel"}),"quiet");
      this.hint=element("p","hint");const actions=element("div","actions");actions.append(this.cancel,this.send);
      this.composer.append(this.input,actions,this.hint);
      this.node.append(header,this.controls,this.permissions,this.transcript,this.source,this.notice,this.composer);
      document.getElementById("panes").append(this.node);
    }
    async flush() { save(this.id,this.input.value); }
    reset() {save(this.id,this.input.value);this.id=undefined;this.input.value="";this.generation=undefined;this.data=undefined;}
    action(fields) {return command({pane:this.index,generation:this.generation,...fields});}
    async control(value) {await this.action({action:"external_control",command:value});}
    async submit() {
      if (this.switching || this.submitting || this.uncertain || !this.input.value.trim() || !this.data?.capabilities.submit) return;
      const text=this.input.value;this.submitting=true;this.renderComposer();
      try {await this.control({kind:"submit",text});if(this.input.value===text){this.input.value="";save(this.id,"");}}
      catch(error) {this.uncertain=true;throw error;}
      finally {this.submitting=false;this.renderComposer();}
    }
    renderComposer() {
      this.input.disabled=!!this.switching;
      this.send.disabled=!!(this.switching||this.submitting||this.uncertain||this.data?.busy||!this.data?.capabilities.submit||!this.input.value.trim());
      this.cancel.disabled=!!(this.data?.busy||!this.data?.observed.connected||this.data?.observed.snapshot.status!=="running");
    }
    render(data) {
      if(!data?.external)return;
      const state=data.external, observed=state.observed, snapshot=observed.snapshot;
      if(this.generation!==data.generation){save(this.id,this.input.value);this.generation=data.generation;this.id=snapshot.id;this.input.value=drafts.get(this.id)??"";this.uncertain=false;this.switching=false;this.blocks.clear();this.transcript.replaceChildren();}
      this.data=state;
      this.name.textContent=`${snapshot.endpoint} · External agent`;
      this.path.textContent=`${snapshot.cwd} · ${snapshot.id}`;
      const status=observed.connected?snapshot.status:(snapshot.status==="closed"?"Closed":"Unknown · disconnected");
      this.status.textContent=`${status}${snapshot.completion?` · ${snapshot.completion.replaceAll("_"," ")}`:""} · Local observations`;
      this.status.dataset.connected=String(observed.connected);
      this.notice.textContent=this.uncertain?"The send outcome is unknown. Your text is retained. Check the conversation before writing another prompt.":state.diagnostic??"";
      this.notice.hidden=!this.notice.textContent;
      for(const control of this.controls.children)control.disabled=state.busy;
      this.next.disabled=state.busy||!state.more;
      this.live.disabled=state.busy||state.following;
      this.resume.hidden=!state.capabilities.resume;this.resume.disabled=state.busy||observed.connected;
      this.load.hidden=!state.capabilities.load;this.load.disabled=state.busy||observed.connected;
      this.close.disabled=state.busy||!observed.connected;
      const permissionKey=JSON.stringify([snapshot.generation,observed.permissions,state.busy]);
      if(permissionKey!==this.permissionKey){this.permissionKey=permissionKey;this.permissions.replaceChildren();for(const pending of observed.permissions){
        const row=element("section","external-permission");const title=element("h3","",pending.title);const options=element("div","actions");
        row.dataset.request=pending.id;row.dataset.generation=pending.generation;row.tabIndex=-1;
        for(const option of pending.options){const choice=button(`${option.name} · ${option.kind.replaceAll("_"," ")}`,()=>this.control({kind:"answer",generation:pending.generation,permission:pending.id,option:option.id}),"quiet");choice.disabled=state.busy;options.append(choice);}
        row.append(title,options);this.permissions.append(row);
      }}
      const focus=data.attention_focus;const focusKey=JSON.stringify([data.generation,focus]);
      if(focus?.kind==='external' && this.focusKey!==focusKey){this.focusKey=focusKey;const row=[...this.permissions.children].find(row=>row.dataset.request===focus.request&&row.dataset.generation===focus.generation);if(row){row.scrollIntoView({block:'nearest'});row.focus();}}
      const bottom=this.transcript.scrollHeight-this.transcript.scrollTop-this.transcript.clientHeight<80;
      const keys=new Set(state.blocks.map(block=>block.key));for(const [key,row] of this.blocks)if(!keys.has(key)){row.remove();this.blocks.delete(key);}
      for(const block of state.blocks){if(this.blocks.has(block.key))continue;const row=element("article","external-record");const label=element("div","external-record-label",block.role);const text=element("pre","",block.text);const source=button(block.truncated?"Read full source":"Source",()=>this.action({action:"external_source",source:block.source,start:0}),"quiet");row.append(label,text,source);this.transcript.append(row);this.blocks.set(block.key,row);}
      if(bottom && state.following)this.transcript.scrollTop=this.transcript.scrollHeight;
      const raw=data.external_source;const sourceKey=JSON.stringify(raw);if(sourceKey!==this.sourceKey){this.sourceKey=sourceKey;this.source.replaceChildren();this.source.hidden=!raw;if(raw){this.source.append(element("h3","",`Observed source · bytes ${raw.start}–${raw.end}`),element("pre","",raw.text));if(raw.more)this.source.append(button("Next source window",()=>this.action({action:"external_source",source:raw.source,start:raw.end}),"quiet"));}}
      this.renderComposer();
    }
  };
}
