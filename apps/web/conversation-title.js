import {workspaceName} from './workspace-name.js';

export function navigationTitle(view, session) {
  const direct = view?.navigation?.entries?.find(entry=>entry.session===session);
  if (direct) return direct.metadata.title;
  for (const group of Object.values(view?.navigation?.groups ?? {})) {
    const entry = group.entries.find(entry=>entry.session===session);
    if (entry) return entry.metadata.title;
  }
}

export function conversationTitle(view, key) {
  const surface = view?.surfaces[key];
  if (surface?.kind === 'native') {
    const workspace = surface.workspace;
    return navigationTitle(view,surface.session) || workspaceName(workspace.coordinates.path,workspace.registration.status==='registered'?workspace.registration.name:null) || 'New conversation';
  }
  if (surface?.kind === 'external') return workspaceName(surface.external.observed.snapshot.cwd) || 'External conversation';
  return key === 'main' ? 'New conversation' : key === 'compare' ? 'Compare' : key;
}
