async function downloadWorker() {
  if (!navigator.serviceWorker) throw new Error("Streaming downloads require a secure browser origin with Service Workers");
  const registration = await navigator.serviceWorker.register("/download-worker.js", {scope:"/downloads/", updateViaCache:"none"});
  const worker = registration.installing ?? registration.waiting ?? registration.active;
  if (!worker) throw new Error("Download worker is unavailable");
  if (worker.state !== "activated") await new Promise((resolve,reject) => {
    const timer = setTimeout(() => { worker.removeEventListener("statechange",changed); reject(new Error("Download worker did not activate")); },30000);
    function changed() { if (worker.state === "activated" || worker.state === "redundant") { clearTimeout(timer); worker.removeEventListener("statechange",changed); worker.state === "activated" ? resolve() : reject(new Error("Download worker was replaced")); } }
    worker.addEventListener("statechange",changed); changed();
  });
  return worker;
}

// At most one 64 KiB chunk is outstanding. Never collect or Blob the artifact.
export async function downloadSession(call, pane, generation, args, signal) {
  if (location.protocol === "rsi:") {
    const cancel = () => { void call("export_cancel", "").catch(()=>{}); };
    signal.addEventListener("abort",cancel,{once:true});
    try {
      if (signal.aborted) throw new Error("Export cancelled");
      return JSON.parse(await call("export_save",JSON.stringify({pane,generation,arguments:args})));
    } finally { signal.removeEventListener("abort",cancel); }
  }
  const worker = await downloadWorker();
  if (signal.aborted) throw new Error("Export cancelled");
  const invoke = operation => call("export_input",JSON.stringify({pane,generation,operation})).then(JSON.parse);
  const opened = await invoke({kind:"open",arguments:args});
  const channel = new MessageChannel();
  const id = [...crypto.getRandomValues(new Uint8Array(32))].map(n=>n.toString(16).padStart(2,"0")).join("");
  let pulling = false, ended = false, anchor, frameReady;
  try {
    await new Promise((resolve,reject) => {
      let pendingFailure, failureTimer;
      const done = failure => { clearTimeout(failureTimer); signal.removeEventListener("abort",abort); failure ? reject(failure) : resolve(); };
      const fail = failure => {
        if (pendingFailure) return;
        pendingFailure = failure;
        // Interrupt the Rust read immediately, but keep the port/frame alive
        // until the worker acknowledges erroring the browser response.
        void invoke({kind:"cancel",token:opened.token}).catch(()=>{});
        channel.port1.postMessage({kind:"abort",error:String(failure)});
        failureTimer = setTimeout(()=>done(failure),2000);
      };
      const abort = () => fail(new Error("Export cancelled"));
      signal.addEventListener("abort",abort,{once:true});
      channel.port1.onmessageerror = () => done(new Error("Download channel failed"));
      channel.port1.onmessage = async ({data}) => {
        try {
          if (data.kind === "aborted") { done(pendingFailure ?? new Error("Export cancelled")); return; }
          if (pendingFailure) return;
          if (data.kind === "ready") {
            if (data.path !== `/downloads/${id}`) throw new Error("Download identity changed");
            // A navigation selects the download-scoped worker even though the application page is uncontrolled.
            anchor = document.createElement("iframe"); anchor.hidden = true;
            frameReady = event => {
              if (event.origin === location.origin && event.source === anchor.contentWindow && event.data?.kind === "rsi-export-frame" && event.data.id === id) {
                window.removeEventListener("message",frameReady);
                channel.port1.postMessage({kind:"bind",client:event.data.client});
              }
            };
            window.addEventListener("message",frameReady);
            anchor.src = data.path; document.body.append(anchor);
          } else if (data.kind === "bound") {
            if (!/^[a-f0-9]{64}$/.test(data.nonce)) throw new Error("Invalid download authorization");
            anchor.contentWindow.postMessage({kind:"rsi-export-download",id,nonce:data.nonce},location.origin);
          } else if (data.kind === "pull") {
            if (pulling || ended) throw new Error("Invalid download demand");
            pulling = true;
            const item = await invoke({kind:"next",token:opened.token});
            if (pendingFailure) return;
            if (item?.type === "chunk") {
              const bytes = new TextEncoder().encode(item.text);
              if (!bytes.byteLength || bytes.byteLength>65536) throw new Error("Invalid export chunk");
              channel.port1.postMessage({kind:"chunk",bytes:bytes.buffer},[bytes.buffer]);
            } else if (item?.type === "complete") {
              if (await invoke({kind:"next",token:opened.token}) !== null) throw new Error("Export continued after completion");
              ended = true; channel.port1.postMessage({kind:"end"});
            } else throw new Error("Export ended without completion");
            pulling = false;
          } else if (data.kind === "complete" && ended) done();
          else throw new Error(data.error ?? "Download cancelled");
        } catch (failure) { fail(failure); }
      };
      worker.postMessage({kind:"register",id,filename:opened.filename},[channel.port2]);
      if (signal.aborted) abort();
    });
    return {filename:opened.filename};
  } finally {
    channel.port1.close(); if (frameReady) window.removeEventListener("message",frameReady); anchor?.remove();
    await invoke({kind:"cancel",token:opened.token}).catch(()=>{});
  }
}
