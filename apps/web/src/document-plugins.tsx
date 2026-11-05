import * as React from 'react';
import * as jsx from 'react/jsx-runtime';
import * as dom from 'react-dom';
import * as client from 'react-dom/client';
import type {NativeSurface, TranscriptBlock} from './presentation-types.ts';
import type {InteractionData} from './details/model.ts';
import {resources} from './resource-hosts.ts';
type Question = Extract<InteractionData,{kind:'question'}>['request'];

export interface DocumentSession {
  readonly view: NativeSurface;
  action(name: string, fields?: Record<string, unknown>): Promise<unknown>;
}
export interface DocumentPlugin {
  id: string;
  provider?: React.ComponentType<{children: React.ReactNode}>;
  composer?: React.ComponentType;
  panel?: React.ComponentType;
  review?: React.ComponentType<{request: Question}>;
  turnTail?: React.ComponentType<{turn: string}>;
  transcript?: {
    matches(block: TranscriptBlock): boolean;
    component: React.ComponentType<{block: TranscriptBlock; fallback: React.ReactNode}>;
  };
}
const plugins: DocumentPlugin[] = [];
let sealed = false;
const Session = React.createContext<DocumentSession | undefined>(undefined);

/** Trusted bootstrap composition; registering after mount requires a restart. */
export function registerDocumentPlugin(plugin: DocumentPlugin) {
  if (sealed) throw new Error('Document plugin graph is mounted; restart the application');
  if (!/^[a-zA-Z0-9._-]{1,128}$/.test(plugin.id) || plugins.length >= 16 || plugins.some(item => item.id === plugin.id))
    throw new Error('Invalid or duplicate document plugin');
  plugins.push(Object.freeze({...plugin, transcript: plugin.transcript && Object.freeze({...plugin.transcript})}));
}
export function sealDocumentPlugins() { sealed = true; }
export const documentModules = Object.freeze({react: React, 'react/jsx-runtime': jsx, 'react-dom': dom, 'react-dom/client': client});
export function useOptionalDocumentSession() { return React.useContext(Session); }
export function useDocumentSession() {
  const session = React.useContext(Session);
  if (!session) throw new Error('Document contribution requires a Session occurrence');
  return session;
}
export function DocumentSessionScope({view, owner, children}: {
  view?: NativeSurface | null;
  owner: {staged?: {data: NativeSurface | null}; action(name: string, fields?: Record<string, unknown>): Promise<unknown>};
  children: React.ReactNode;
}) {
  const lifetime = React.useMemo(() => ({active: true}), [view?.session, view?.generation]);
  React.useLayoutEffect(() => {
    lifetime.active = true;
    return () => { lifetime.active = false; };
  }, [lifetime]);
  const scope = view && {
    view,
    action(name: string, fields: Record<string, unknown> = {}) {
      if (!lifetime.active || owner.staged?.data?.session !== view.session || owner.staged.data.generation !== view.generation)
        return Promise.reject(new Error('Document Session occurrence retired'));
      if ('pane' in fields || 'generation' in fields) return Promise.reject(new Error('Document action cannot replace its Session binding'));
      return owner.action(name, fields);
    },
  };
  const content = plugins.reduceRight((content, plugin) => {
    const Provider = plugin.provider;
    return Provider ? <Provider key={plugin.id}>{content}</Provider> : content;
  }, children);
  return <Session.Provider value={scope || undefined}>{content}</Session.Provider>;
}
class ContributionBoundary extends React.Component<{children: React.ReactNode; fallback?: React.ReactNode}, {error?: string}> {
  state: {error?: string} = {};
  static getDerivedStateFromError(error: unknown) {
    return {error: String(error instanceof Error ? error.message : error).slice(0, 2048)};
  }
  render() {
    return this.state.error ? <>{this.props.fallback}<p role="alert">Extension view unavailable: {this.state.error}</p></> : this.props.children;
  }
}

export function DocumentContributions({seat, turn}: {seat: 'composer' | 'panel' | 'turnTail'; turn?: string}) {
  const session = React.useContext(Session);
  if (!session) return null;
  return <>{plugins.map(plugin => {
    const Component = plugin[seat];
    return Component ? <ContributionBoundary key={`${plugin.id}:${session.view.session}:${session.view.generation}:${turn ?? ''}`}><Component turn={turn ?? ''}/></ContributionBoundary> : null;
  })}</>;
}
export function DocumentTranscript({block, children}: {block: TranscriptBlock; children: React.ReactNode}) {
  const session = React.useContext(Session);
  if (!session) return children;
  const plugin = plugins.find(item => item.transcript?.matches(block));
  const Component = plugin?.transcript?.component;
  return Component ? <ContributionBoundary key={`${plugin!.id}:${session.view.generation}:${block.key}`} fallback={children}><Component block={block} fallback={children}/></ContributionBoundary> : children;
}

export function DocumentReviews() {
  const owner = React.useContext(Session);
  const hosts = React.useSyncExternalStore(resources.subscribe, resources.getSnapshot);
  if (!owner || owner.view.protected) return null;
  return <>{hosts.map(({entry,model}) => {
    if (entry.session !== owner.view.session || entry.generation !== owner.view.generation || model?.kind !== 'interaction') return null;
    const {data} = model;
    if (data.kind !== 'question' || !data.request.review || data.generation !== owner.view.generation
      || data.request.session_id !== owner.view.session || !owner.view.pending?.some(item => item.kind === 'question' && item.id === data.request.id)) return null;
    return plugins.map(({id,review:Component}) => Component && <ContributionBoundary key={`${id}:${data.ticket}`}><Component request={data.request}/></ContributionBoundary>);
  })}</>;
}
