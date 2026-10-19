import { keyboardAction } from "./composer-actions.js";
import { externalDrafts } from "./external-drafts.js";
// Display-only external conversations; Rust owns observation and one-shot controls.
export function externalPaneClass({ element, button, command, perform }) {
  const drafts = externalDrafts();
  return class ExternalPane {
    static clearDrafts() { drafts.clear(); }
    kind = "external";
    blocks = new Map();
    changed = () => {
      if(this.record && (this.record.revision!==this.capacityRevision || !drafts.current(this.record)))this.capacity=false;
      const text = this.record?.text;
      if (text !== undefined && this.input.value !== text) {
        const {selectionStart,selectionEnd} = this.input;
        this.input.value = text;
        this.input.setSelectionRange?.(Math.min(selectionStart,text.length),Math.min(selectionEnd,text.length));
      }
      this.renderComposer();
    };
    get uncertain() { return !!this.record && this.record.unknownRevision === this.record.revision; }
    get submitting() { return !!this.record?.pending; }
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
      this.input.addEventListener("input",()=>this.save());
      this.input.addEventListener("keydown",event=>{if(keyboardAction(event,this.submitKey??"enter")){event.preventDefault();perform(()=>this.submit());}});
      this.composer.addEventListener("submit",event=>{event.preventDefault();perform(()=>this.submit());});
      this.send=button("Send ↗",()=>this.submit(),"primary");
      this.send.dataset.testid="composer-send";
      this.cancel=button("Cancel prompt",()=>this.control({kind:"cancel"}),"quiet");
      this.hint=element("p","hint");const actions=element("div","actions");actions.append(this.cancel,this.send);
      this.composer.append(this.input,actions,this.hint);
      this.node.append(header,this.controls,this.permissions,this.transcript,this.source,this.notice,this.composer);
      document.getElementById("panes").append(this.node);
    }
    save() {
      const accepted=drafts.edit(this.record,this.input.value);
      if(!accepted) {this.capacity=true;this.capacityRevision=this.record?.revision;this.changed();}
      return accepted;
    }
    async flush() { if(this.id) this.save(); }
    reset() {if(this.id)this.save();drafts.unbind(this.record,this.changed);this.record=null;this.id=undefined;this.input.value="";this.generation=undefined;this.data=undefined;this.capacity=false;this.bindingVersion=undefined;this.renderComposer();}
    action(fields) {return command({pane:this.index,generation:this.generation,...fields});}
    async control(value) {await this.action({action:"external_control",command:value});}
    async submit() {
      if (this.switching || this.submitting || !this.data?.capabilities.submit || this.data?.busy || !this.save() || this.uncertain || !this.input.value.trim()) return;
      const attempt=drafts.begin(this.record);if(!attempt)return;
      try {await this.control({kind:"submit",text:attempt.text});drafts.finish(attempt,true);}
      catch(error) {drafts.finish(attempt,false);throw error;}
    }
    renderComposer() {
      this.input.disabled=!!this.switching||!drafts.current(this.record);
      this.send.disabled=!!(this.switching||this.submitting||this.uncertain||!drafts.current(this.record)||this.data?.busy||!this.data?.capabilities.submit||!this.input.value.trim());
      this.cancel.disabled=!!(this.data?.busy||!this.data?.observed.connected||this.data?.observed.snapshot.status!=="running");
      const notice=[this.uncertain?"The send outcome is unknown. Your text is retained. Check the conversation before writing another prompt.":"",this.capacity?(this.record?"External draft capacity is full. Existing text is retained.":"External draft capacity is full. Close another pane to enable composition."):""].filter(Boolean).join(" ")||this.data?.diagnostic||"";
      if(this.notice.textContent!==notice)this.notice.textContent=notice;
      if(this.notice.hidden!==!notice)this.notice.hidden=!notice;
    }
    render(data) {
      if(!data?.external)return;
      const state=data.external, observed=state.observed, snapshot=observed.snapshot;
      if(this.generation!==data.generation || this.id!==snapshot.id || (this.record && !drafts.current(this.record))){
        if(this.id && drafts.current(this.record))this.save();drafts.unbind(this.record,this.changed);
        this.generation=data.generation;this.id=snapshot.id;this.record=drafts.bind(this.id,this.changed);this.capacity=!this.record;
        this.bindingVersion=drafts.version();
        this.input.value=this.record?.text??"";this.switching=false;this.blocks.clear();this.transcript.replaceChildren();
      }
      if(!this.record && this.bindingVersion!==drafts.version()) {
        this.record=drafts.bind(this.id,this.changed);this.capacity=!this.record;this.bindingVersion=drafts.version();
        if(this.record)this.input.value=this.record.text;
      }
      this.data=state;
      this.name.textContent=`${snapshot.endpoint} · External agent`;
      this.path.textContent=`${snapshot.cwd} · ${snapshot.id}`;
      const status=observed.connected?snapshot.status:(snapshot.status==="closed"?"Closed":"Unknown · disconnected");
      this.status.textContent=`${status}${snapshot.completion?` · ${snapshot.completion.replaceAll("_"," ")}`:""} · Local observations`;
      this.status.dataset.connected=String(observed.connected);
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
