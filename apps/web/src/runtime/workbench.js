import {createSettingsRuntime} from './settings-runtime.js';
import {createResourceDetails} from './resource-details.js';
import {sessionSurfaces} from '../session/state.ts';
import {createViewProjector} from './view-projector.js';
import {createResourceProjector} from './resource-projector.js';
import {createDocumentClose} from './document-close.js';
import {createPaneClass} from './session-controller.js';
import {reactPresentation} from '../frame-commit.tsx';
import {createFramePresenter} from './frame-presenter.js';
import { resourceHosts, publishResources, clearResources } from "../resource-hosts.ts";
const documentBuildFamily = typeof __RSI_BUILD_FAMILY__ === "string" ? __RSI_BUILD_FAMILY__ : "";
import { keyboardAction } from "../../composer-actions.js";
import { completionToken } from "../../completion-token.js";
import { TranscriptVisibility } from "../../transcript-visibility.js";
import { presentationIdentity } from "../presentation.tsx";
import { presentationKey } from "../../presentation-store.js";
import { downloadSession } from "../../session-export.js";
import { DocumentConnection } from "../../document-connection.js";
import {installDisclosures} from '../../disclosure.js';
import { settingsPanel, publish, installActions, selectSurface,connectionStatus,setNotice,setStorageNotice } from "../bridge.ts";
import { createDocumentTransport } from "../native.ts";
import { isDesktop, desktopCall } from "../../desktop-bridge.js";
import { MountTable, writeClipboard, setClipboardWriter } from "/mounts.js";
import { DraftStore, DraftEditor, validateEditor } from "/drafts.js";
import { externalPaneClass } from "./external-controller.js";
import { ImagePreviews } from "../../image-previews.js";
function referenceOrigin(meta) {
  return meta.source.kind === "native" ? `Session ${meta.source.binding.session_id}` : `${meta.source.owner}/${meta.source.id} · observed epoch ${meta.source.epoch}`;
}
function referenceInterval(meta) {
  const capture = meta.capture;
  return capture.kind === "suffix" ? {...capture.interval, first:String(BigInt(capture.interval.retained_after_seq)+1n), last:capture.interval.retained_through_seq} : {through_seq:capture.selection.through_seq,first:capture.selection.record.sequence,last:capture.selection.record.sequence,omissions:[]};
}
export function createWorkbench({
  transportFactory = createDocumentTransport,
  mountTable = MountTable,
  draftStore = DraftStore,
  presentation = reactPresentation,
  nativeDocument = isDesktop(),
  nativeCall = desktopCall,
} = {}) {
const events = new AbortController();
const subscriptions = [];
const listen = (target, name, listener) => target.addEventListener(name, listener, {signal:events.signal});
subscriptions.push(installDisclosures(document));
const $ = id => document.getElementById(id);
let localLaunch;
if (location.hash.startsWith("#rsi-launch=")) {
  const fragment = location.hash;
  history.replaceState(null, "", location.pathname + location.search);
  localLaunch = /^#rsi-launch=([a-f0-9]{64})$/.exec(fragment)?.[1] ?? "invalid";
}
let localBootstrap = false;
try { localBootstrap = localStorage.getItem("rsi.local-web") === "1"; } catch { /* Optional browser storage. */ }
let connection;
let connecting = false;
let view;
let rendererSlots = [];
let selected = "main";
let lastNotice;
let imagePreviews, imageConnection;
function clearImages(){imagePreviews?.dispose();imagePreviews=imageConnection=undefined}
function acquireImage(owner,media,ticket,call){
 if(owner!==connection||owner?.closing)throw Error('The image connection is closed');
 if(!imagePreviews||imageConnection!==owner){clearImages();imageConnection=owner;imagePreviews=new ImagePreviews(call,view.media_limits)}
 return imagePreviews.acquire(media,ticket);
}
let endpoint;
try { endpoint = localStorage.getItem("rsi.endpoint"); } catch { /* Storage is optional. */ }

const setConnection=patch=>connectionStatus.set({...connectionStatus.getSnapshot(),...patch});
connectionStatus.set({phase:'Disconnected',native:nativeDocument,reconnect:!!endpoint,busy:false,closing:false,developmentHttp:!nativeDocument&&location.protocol==='http:'});

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function notify(message) {
  const storage = connection && !connection.closing ? connection.storageNotice : undefined;
  setStorageNotice(storage ?? "");
  setNotice(message ?? "");
}
async function perform(run) {
  try { await run(); } catch (error) { notify(String(error.message ?? error)); }
}
function clearView() {
  presentation.retire();
  setStorageNotice("");
  settings.clear();
  presentationIdentity.set(undefined); publish(undefined); view = undefined; lastNotice = undefined;
  clearImages(); clearResources();
  for (const pane of panes.values()) pane.reset();
  ExternalPane.clearDrafts();
}
function failWorker(error, current = connection) { current?.fail(new Error(error)); }
function connectionFailed(error, current) {
  if (current !== connection) return;
  clearView();
  setConnection({phase:'Connection failed',closing:false});
  notify(`${error.message}. Reconnect explicitly after renderer cleanup succeeds.`);
}

const frames=createFramePresenter({get connection(){return connection},get view(){return view},get panes(){return panes},get rendererSlots(){return rendererSlots},render,presentation,$,notify});
const presentFrame=frames.presentFrame;
function createDocumentConnection(transport, table) {
  const current = new DocumentConnection({
    transport, mounts: table,
    isCurrent: () => connection === current,
    onFrame: (frame, assets) => presentFrame(frame, assets, current),
    onFailure: error => connectionFailed(error, current),
    onCleanupFailure: error => notify(`Connection cleanup failed: ${error.message}`),
  });
  return current;
}
async function makeWorker() {
  presentation.retire();
  await connection?.retire();
  const table = await mountTable.open();
  let transport;
  try { transport = transportFactory(); }
  catch (error) { await table.close(); throw error; }
  const current = createDocumentConnection(transport, table);
  connection = current;
  for (const pane of panes.values()) pane.switching = false;
  frames.reset();
  return current;
}
function call(method, payload, transfer = []) {
  return boundCall(connection)(method, payload, transfer);
}
function boundCall(owner) {
  return (method, payload, transfer = []) => owner
    ? owner.request(method, payload, transfer)
    : Promise.reject(Object.assign(new Error("Connect to your service first"), { notAdmitted: true, retryable: false }));
}

const settings=createSettingsRuntime({get connection(){return connection},get view(){return view},boundCall,notify});
const command=settings.command;

function completeConnection(current, identity, local) {
    if (!identity || connection !== current || current.closing) return;
    if (local) {
      $("dev-http").checked = true;
      try { localStorage.setItem("rsi.local-web", "1"); } catch { /* Optional browser storage. */ }
    }
    endpoint = identity.endpoint_id;
    presentationIdentity.set(presentationKey(endpoint,identity.principal));
    try { localStorage.setItem("rsi.endpoint", endpoint); } catch { /* Storage is optional. */ }
    setConnection({phase:'Connected',closing:false,reconnect:true});
    if (current.storageNotice) notify("");
    void current.request("command", JSON.stringify({action:"initialize_workspace"})).catch(error => { if (connection === current && !current.closing) notify(error.message); });
}
async function connectWith(receipt, localTicket) {
  if (connecting || (connection && !connection.closing)) throw new Error("A connection is already active or opening");
  connecting = true;
  setConnection({phase:'Connecting…',busy:true});
  notify('');
  let current;
  try {
    current = await makeWorker();
    const local = localTicket !== undefined;
    const payload = local ? { localBootstrap: true, ticket: localTicket } : { receipt, devHttp: $("dev-http").checked };
    payload.buildFamily = documentBuildFamily;
    let identity;
    try {
      identity = await current.authenticate(payload, async identity => {
        try { return { drafts: await draftStore.open(identity.endpoint_id, identity.principal) }; }
        catch (error) {
          return { drafts: new DraftStore(undefined, identity.endpoint_id, identity.principal.kind === "local" ? "local" : `device:${identity.principal.device_id}`),
            storageNotice: `Draft storage is unavailable. Input cannot be saved or sent: ${error.message}` };
        }
      });
    } finally { delete payload.ticket; localTicket = undefined; }
    completeConnection(current, identity, local);
  } catch (error) {
    if (current) failWorker(error.message, current);
    else notify(error.message);
  } finally {
    connecting = false;
    if (!current || connection === current) setConnection({busy:false});
  }
}
listen($("login-form"), "submit", event => {
  event.preventDefault();
  const receipt = $("receipt").value;
  $("receipt").value = "";
  if (receipt.length > 2048) { notify("Device receipt exceeds its limit"); return; }
  perform(() => connectWith(receipt));
});

listen($("reconnect"), "click", () => perform(() => connectWith(JSON.stringify({ endpoint_id: endpoint }))));
const closer=createDocumentClose({
 get connection(){return connection},get panes(){return panes},nativeCall,nativeDocument,notify,failWorker,
 setClosing(value){setConnection({closing:value})},
 disconnected(resources){document.dispatchEvent(new CustomEvent("rsi-disconnected",{detail:resources}));clearView();setConnection({phase:'Disconnected',closing:false});notify('')},
});
const disconnectDocument=closer.disconnectDocument;
listen(window, "rsi-native-close", event => { if (!nativeDocument || !Number.isSafeInteger(event.detail)) return; closer.attempt = event.detail; perform(disconnectDocument); });
if(nativeDocument)setClipboardWriter(text=>nativeCall({kind:"clipboard",text}));

const Pane = createPaneClass({openModels:()=>settingsPanel.set("models"), DraftEditor, DraftStore, TranscriptVisibility, boundCall, call, command, completionToken, get connection() {return connection}, downloadSession, keyboardAction, location, notify, perform, referenceInterval, referenceOrigin, get rendererSlots() {return rendererSlots}, select, validateEditor, get view() {return view}});
const panes = new Map();
listen(window, "rsi:detail-mode",()=>{for(const pane of panes.values())if(pane.lastTranscript)pane.renderTranscript(pane.lastTranscript,false);});
const ExternalPane = externalPaneClass({command,perform});
function select(index) {
  selected = index;
  selectSurface(index);
}
async function openInSelected(fields) {
  const owner = connection;
  const pane = panes.get(selected);
  if (!pane) throw new Error("Open a conversation surface first");
  if (pane.switching || pane.submitting || pane.uploading) throw new Error("Wait for this conversation’s current operation");
  pane.switching = true;
  pane.renderComposer();
  try {
    await pane.flush();
    const editor = pane.editor;
    const reuse = fields.action === "create" && editor && !editor.dirty && !editor.failure && !editor.record.pending && !editor.text && !editor.images.length && !editor.references.length && !pane.submitting && !pane.uploading
      ? {generation:pane.generation,header:editor.record.header} : undefined;
    await owner.request("command", JSON.stringify({ ...fields, pane: pane.index, ...(reuse ? {reuse} : {}) }));
    if (connection === owner && fields.action === "new_conversation" && !pane.generation) { pane.switching = false; pane.renderComposer(); }
  } catch (error) {
    if (connection !== owner) throw error;
    pane.switching = false;
    pane.render(view?.surfaces[pane.index], view?.catalog.models ?? []);
    throw error;
  }
}
const projectView=createViewProjector({publishSurfaces:sessionSurfaces.set,get rendererSlots(){return rendererSlots},set rendererSlots(value){rendererSlots=value},get view(){return view},set view(value){view=value},get lastNotice(){return lastNotice},set lastNotice(value){lastNotice=value},get notify(){return notify},get panes(){return panes},get ExternalPane(){return ExternalPane},get Pane(){return Pane},get selected(){return selected},set selected(value){selected=value},get select(){return select},get publish(){return publish},renderSettings:settings.present,get renderResources(){return renderResources}});
function render(next){return projectView(next)}
installActions({ disconnect:async()=>nativeDocument?nativeCall({kind:"request_close"}):disconnectDocument(), restoreDrafts:async()=>{const pane=panes.get(selected);if(!(pane instanceof Pane))throw new Error("Open a local conversation first");await pane.restoreDrafts()}, command, open: openInSelected, select, call, resources: () => connection?.resources, dismissSettings:settings.dismiss,
  async closeSurface(key) {
    const owner = connection;
    const pane = panes.get(key);
    if (pane?.switching || pane?.submitting || pane?.uploading) throw new Error("Wait for this conversation's current operation before closing");
    if (pane) { pane.switching = true; pane.renderComposer(); }
    try { if (pane) await pane.flush(true); await owner.request("command", JSON.stringify({action:"close_surface",pane:key})); }
    catch (error) { if (pane && connection === owner) { pane.switching = false; pane.renderComposer(); } throw error; }
  },
  async addSurface(key) {
    const owner = connection;
    await boundCall(owner)("command", JSON.stringify({action:"add_surface",pane:key}));
    if (connection === owner) select(key);
  },
});

const projectResources=createResourceProjector({get connection(){return connection},get resourceHosts(){return resourceHosts},get element(){return element},get $(){return $},get renderDetail(){return renderResourceDetail},get publishResources(){return publishResources}});
function renderResources(next){return projectResources(next)}
const renderResourceDetail=createResourceDetails({get connection(){return connection},boundCall,acquireImage,get rendererSlots(){return rendererSlots}});

return {
  start() {
if (nativeDocument) {
  perform(() => nativeCall({kind:"hello"}));
}
if (localLaunch !== undefined || (localBootstrap && location.protocol === "http:")) {
  const ticket = localLaunch ?? null;
  localLaunch = undefined;
  perform(() => connectWith(undefined, ticket));
}

  },
  connect: connectWith,
  async attach(transport, table, payload, openDrafts) {
    presentation.retire(); await connection?.retire();
    connection = createDocumentConnection(transport, table); frames.reset();
    const current = connection;
    const identity = await current.authenticate(payload, openDrafts);
    completeConnection(current, identity, false);
    return current;
  },
  get connection() { return connection; },
  get view() { return view; },
  get panes() { return panes; },
  presentFrame, command, call, select, disconnect: disconnectDocument,
  project: render,
  async dispose() {
    presentation.retire(); events.abort(); settings.dispose();
    for (const unsubscribe of subscriptions.splice(0)) unsubscribe();
    await connection?.retire(); clearView();
    for (const pane of panes.values()) pane.dispose();
    panes.clear();sessionSurfaces.set([]);
  },
};
}
