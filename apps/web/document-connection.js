import { lane, limits } from "./admission.js";

// Bound the document waiter; an expired native operation may still own work.
function closeStage(operation, stage) {
  let timer;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${stage} did not complete within 30 seconds`)), 30_000);
  });
  return Promise.race([operation, deadline]).finally(() => clearTimeout(timer));
}

// One document generation owns RPC admission, authentication and final cleanup.
export class DocumentConnection {
  #transport;
  #mounts;
  #pending = new Map();
  #inFlight = Object.fromEntries(Object.keys(limits).map(selected => [selected, 0]));
  #nextId = 0;
  #phase = "Opening";
  #authenticated;
  #authenticationDone;
  #teardown;
  #mountClose;
  #termination;
  #frameTask;
  #disconnectTask;
  #isCurrent;
  #onFrame;
  #onFailure;
  #onCleanupFailure;

  constructor({ transport, mounts, isCurrent, onFrame, onFailure, onCleanupFailure }) {
    this.#transport = transport;
    this.#mounts = mounts;
    this.#isCurrent = isCurrent;
    this.#onFrame = onFrame;
    this.#onFailure = onFailure;
    this.#onCleanupFailure = onCleanupFailure;
    this.#authenticated = new Promise(resolve => { this.#authenticationDone = resolve; });
    transport.onmessage = ({ data }) => this.#receive(data);
    transport.onerror = event => { event.preventDefault(); this.fail(new Error("Document transport stopped")); };
  }

  get closing() { return !["Opening", "Ready"].includes(this.#phase); }
  get phase() { return this.#phase; }
  render(assets, slots) { return this.#mounts.render(assets, slots); }
  settled() {
    return ["Opening", "Draining"].includes(this.#phase) && this.#disconnectTask
      ? this.#disconnectTask : this.#teardown ?? Promise.resolve();
  }

  retire() {
    if (!["Failed", "Closed"].includes(this.#phase)) {
      this.#phase = "Closed";
      this.#authenticationDone();
      this.#rejectPending(new Error("The application connection was replaced"));
    }
    return this.#close();
  }

  request(method, payload, transfer = []) {
    const selected = lane(method, payload);
    const draining = this.#phase === "Draining" && method === "disconnect";
    if ((!draining && this.closing) || !this.#isCurrent()) {
      return Promise.reject(Object.assign(new Error("The application connection is closed"), { notAdmitted: true, retryable: false }));
    }
    if (this.#inFlight[selected] >= limits[selected]) {
      return Promise.reject(Object.assign(new Error("Input is busy; wait for the current action"), { notAdmitted: true }));
    }
    const id = ++this.#nextId;
    return new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject, lane: selected });
      this.#inFlight[selected]++;
      try { this.#transport.postMessage({ kind: "call", id, method, payload }, transfer); }
      catch (error) { this.#takePending(id); reject(Object.assign(error, { notAdmitted: true, retryable: false })); }
    });
  }

  async authenticate(payload, openDrafts) {
    try {
      const identity = JSON.parse(await this.request("connect", payload));
      if (this.closing || !this.#isCurrent()) return;
      const storage = await openDrafts(identity);
      if (this.closing || !this.#isCurrent()) return;
      this.drafts = storage.drafts;
      this.storageNotice = storage.storageNotice;
      this.#phase = "Ready";
      return identity;
    } catch (error) {
      const failure = error instanceof Error ? error : new Error(String(error?.message ?? error ?? "Authentication failed"));
      this.fail(failure);
      throw failure;
    } finally { this.#authenticationDone(); }
  }

  #takePending(id) {
    const waiter = this.#pending.get(id);
    if (waiter) {
      this.#pending.delete(id);
      this.#inFlight[waiter.lane]--;
    }
    return waiter;
  }

  #rejectPending(error) {
    for (const id of this.#pending.keys()) this.#takePending(id).reject(error);
  }

  #terminate() {
    if (!this.#termination) {
      try { this.#termination = closeStage(this.#transport.terminate(), "Transport cleanup"); }
      catch (error) { this.#termination = Promise.reject(error); }
    }
    return this.#termination;
  }

  #close() {
    if (!this.#teardown) {
      this.#teardown = Promise.all([this.#closeMounts(), this.#terminate()]).then(() => {});
      this.#teardown.catch(error => { if (this.#isCurrent()) this.#onCleanupFailure?.(error); });
    }
    return this.#teardown;
  }

  #closeMounts() {
    if (!this.#mountClose) {
      // close() fences renderer ownership synchronously before replacement.
      try { this.#mountClose = closeStage(this.#mounts.close(), "Renderer disposal"); }
      catch (error) { this.#mountClose = Promise.reject(error); }
    }
    return this.#mountClose;
  }

  fail(error) {
    if (["Failed", "Closed"].includes(this.#phase)) return this.settled();
    this.#phase = "Failed";
    this.#authenticationDone();
    this.#rejectPending(error);
    const teardown = this.#close();
    if (this.#isCurrent()) this.#onFailure(error);
    return teardown;
  }

  disconnect(flushCapturedDrafts) {
    if (this.#disconnectTask) return this.#disconnectTask;
    if (!["Opening", "Ready"].includes(this.#phase) || !this.#isCurrent()) return Promise.resolve();
    const opening = this.#phase === "Opening";
    if (!opening) this.#phase = "Draining";
    this.#disconnectTask = (opening ? this.#disconnectOpening(flushCapturedDrafts) : this.#disconnect(flushCapturedDrafts)).finally(() => {
      if (this.#phase === "Ready") this.#disconnectTask = undefined;
    });
    return this.#disconnectTask;
  }

  async #disconnectOpening(flush) {
    try { await closeStage(this.#authenticated, "Authentication before close"); }
    catch (error) { this.fail(error); throw error; }
    if (!this.#isCurrent()) { await this.retire(); return; }
    if (this.#phase !== "Ready") { await this.#close(); return; }
    this.#phase = "Draining";
    return this.#disconnect(flush);
  }

  async #disconnect(flush) {
    try { await closeStage(flush(), "Draft save"); }
    catch (error) {
      if (this.#phase !== "Draining") return;
      if (!this.#isCurrent()) { await this.retire(); return; }
      this.#phase = "Ready";
      throw Object.assign(error instanceof Error ? error : new Error(String(error?.message ?? error ?? "Draft save failed")), { draftSaveFailed: true });
    }
    if (this.#phase !== "Draining") return;
    if (!this.#isCurrent()) { await this.retire(); return; }
    try {
      // Renderer disposal completes before the disconnect receipt is requested.
      await this.#closeMounts();
      if (this.#phase !== "Draining") return;
      if (!this.#isCurrent()) { await this.retire(); return; }
      const resources = await closeStage(this.request("disconnect", true), "Disconnect receipt");
      if (this.#phase !== "Draining") return;
      if (!this.#isCurrent()) { await this.retire(); return; }
      await this.#close();
      if (this.#phase !== "Draining") return;
      if (!this.#isCurrent()) { await this.retire(); return; }
      this.#phase = "Closed";
      this.#authenticationDone();
      this.#rejectPending(new Error("The application disconnected"));
      return resources;
    } catch (error) {
      this.fail(error);
      throw error;
    }
  }

  async #receive(data) {
    if (data.kind === "reply") {
      const waiter = this.#takePending(data.id);
      if (data.error) waiter?.reject(Object.assign(new Error(data.error), { notAdmitted: data.notAdmitted === true, retryable: data.retryable !== false }));
      else waiter?.resolve(data.result);
    } else if (data.kind === "failed") {
      if (!["Failed", "Closed"].includes(this.#phase)) this.fail(new Error(data.error));
    } else if (data.kind === "view" && !this.closing && this.#isCurrent()) {
      if (this.#frameTask) { this.fail(new Error("Overlapping presentation frames")); return; }
      const task = this.#present(data);
      this.#frameTask = task;
      try { await task; }
      finally { if (this.#frameTask === task) this.#frameTask = undefined; }
    }
  }

  async #present(data) {
    await this.#authenticated;
    if (this.closing || !this.#isCurrent()) return;
    try {
      const frame = typeof data.view === "string" ? JSON.parse(data.view) : data.view;
      const assets = typeof data.assets === "string" ? JSON.parse(data.assets) : data.assets;
      const presented = await this.#onFrame(frame, assets);
      if (!this.closing && this.#isCurrent()) this.#transport.postMessage({ kind: "ack", frame_id: frame.frame_id, resync: !presented?.accepted, renderer: presented?.renderer });
    } catch (error) {
      if (!this.closing && this.#isCurrent()) this.fail(new Error(`View rendering failed: ${error.message}`));
    }
  }
}
