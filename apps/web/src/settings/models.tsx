import {useState} from 'react';
import {Button} from '../button.tsx';
import {input,useView,type Provider} from '../bridge.ts';
import {ProviderEditor} from './provider-editor.tsx';
import type {SetupActions} from './setup-actions.ts';
import {providerLabels} from './provider-draft.ts';
import {IconPlusOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './models.module.css';

export function ModelsSettings({actions}:{actions:SetupActions}) {
  const setup=useView(view=>view?.setup);
  const models=useView(view=>view?.catalog.models??[]);
  const more=useView(view=>view?.catalog.models_more);
  const [editing,setEditing]=useState<{entry?:Provider;revision:string;deployments:Provider[]}>();
  const providers=setup?.providers, locked=!setup?.allowed||!setup.providers_writable||actions.busy;
  const edit=(entry?:Provider)=>{if(providers)setEditing({entry,revision:providers.desired_revision,deployments:providers.deployments});};
  return <section className={css.models} aria-label="Models">
    <h2>Models</h2><p className={css.intro}>Enter your API keys to use models from the following providers.</p>
    {!setup?.allowed&&<p role="status">Configuration is read only for this connection.</p>}
    {setup?.allowed&&!setup.providers_writable&&<p role="status">Provider configuration is read only for this connection.</p>}
    {setup?.diagnostic&&setup.diagnostic!==providers?.diagnostic&&<p role="alert" className={css.error}>{setup.diagnostic}</p>}
    {providers?.diagnostic&&<p role="alert" className={css.error}>{providers.diagnostic}</p>}
    {!providers&&<Button disabled={actions.busy} onClick={()=>actions.act(()=>actions.command({kind:'refresh'}))}>Load providers</Button>}
    {providers?.deployments.map(entry=>{
      const id=String(entry.config.deployment),opened=editing?.entry?.config.deployment===id;
      const state=providers.applying?'Applying':providers.desired_revision===providers.applied_revision?'Applied':'Needs attention';
      return <div className={css.card} key={id}>
        <div className={css.cardHeading}><strong>{providerLabels[entry.provider]}{id!==entry.provider&&<span className={css.identity}>{id}</span>}</strong><span className={css.dot} data-state={state} role="img" aria-label={state} title={state}/><Button size="sm" variant="outline" disabled={locked} onClick={()=>opened?setEditing(undefined):edit(entry)}>Edit</Button></div>
        {opened&&<ProviderEditor key={`${id}:${editing.revision}`} original={editing.entry} revision={editing.revision} deployments={editing.deployments} actions={actions} close={()=>setEditing(undefined)}/>}
      </div>;
    })}
    {editing&&!editing.entry?<ProviderEditor key="new" revision={editing.revision} deployments={editing.deployments} actions={actions} close={()=>setEditing(undefined)}/>:<button type="button" className={css.add} disabled={locked||!providers} onClick={()=>edit()}><IconPlusOutlineRegular size={16}/>Add model provider</button>}
    {!!models.length&&<label className={css.defaultModel}>Default model<select aria-label="Default model" disabled={actions.busy||!setup?.settings_writable} value={setup?.agent?.default_model?JSON.stringify(setup.agent.default_model):''} onChange={event=>{if(event.target.value)actions.act(()=>actions.command({kind:'default_model',ticket:setup?.ticket,model:JSON.parse(event.target.value)}));}}><option value="">Choose a model</option>{models.map(model=><option key={`${model.deployment}:${model.model}`} value={JSON.stringify(model)}>{model.model} · {model.deployment}</option>)}</select></label>}
    {more&&<Button size="sm" disabled={actions.busy} onClick={()=>actions.act(()=>input.command({action:"models_next"}))}>More models</Button>}
  </section>;
}
