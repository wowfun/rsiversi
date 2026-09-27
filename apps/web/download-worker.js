// This worker owns only one-use download responses under /downloads/.
// All Session reads and credentials remain in the existing Rust application.
const tickets = new Map();
self.addEventListener("install", event => event.waitUntil(self.skipWaiting()));
self.addEventListener("message", event => {
  if (event.data?.kind === "identify") {
    const client = event.source, id = event.data.id, port = event.ports[0];
    if (port && client?.type === "window" && client.frameType === "nested" && client.url === `${self.location.origin}/downloads/${id}` && tickets.has(id)) port.postMessage({kind:"identity",client:client.id});
    port?.close(); return;
  }
  const { kind, id, filename } = event.data ?? {};
  const port = event.ports[0], client = event.source;
  if (kind !== "register" || !port || !client || client.type !== "window" ||
      new URL(client.url).origin !== self.location.origin ||
      !["/", "/index.html"].includes(new URL(client.url).pathname) ||
      typeof id !== "string" || !/^[a-f0-9]{64}$/.test(id) ||
      typeof filename !== "string" || filename.length > 256 || /[\r\n\0/\\]/.test(filename)) {
    port?.postMessage({ kind: "error", error: "Invalid download registration" }); port?.close(); return;
  }
  if (tickets.size >= 8 || tickets.has(id)) {
    port.postMessage({ kind: "error", error: "Download capacity is full" }); port.close(); return;
  }
  const entry = { port, client: client.id, filename };
  entry.timer = setTimeout(() => { if (tickets.delete(id)) { port.postMessage({kind:"error",error:"Download did not start"}); port.close(); } }, 30000);
  tickets.set(id, entry);
  port.onmessage = async message => {
    if (message.data?.kind === "bind" && typeof message.data.client === "string" && !entry.frame) {
      const frame = await self.clients.get(message.data.client);
      if (!tickets.has(id)) return;
      if (!frame || frame.frameType !== "nested" || frame.url !== `${self.location.origin}/downloads/${id}`) {
        tickets.delete(id); clearTimeout(entry.timer); port.postMessage({kind:"error",error:"Invalid download frame"}); port.close(); return;
      }
      entry.frame = frame.id; entry.nonce = [...crypto.getRandomValues(new Uint8Array(32))].map(n=>n.toString(16).padStart(2,"0")).join("");
      port.postMessage({kind:"bound",nonce:entry.nonce}); return;
    }
    if (message.data?.kind === "abort") { tickets.delete(id); clearTimeout(entry.timer); port.postMessage({kind:"aborted"}); port.close(); }
  };
  port.postMessage({ kind: "ready", path: `/downloads/${id}` });
});
self.addEventListener("fetch", event => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith("/downloads/")) return;
  const parts = url.pathname.slice("/downloads/".length).split("/"), id = parts[0], entry = tickets.get(id);
  if (event.request.method !== "GET" || url.search || !entry || parts.length > 3) {
    event.respondWith(new Response("Download unavailable", {status:404})); return;
  }
  if (parts.length === 1) {
    event.respondWith(new Response('<!doctype html><meta charset="utf-8"><script src="/download-frame.js" defer></script>',{headers:{
      "Content-Type":"text/html; charset=utf-8", "Cache-Control":"no-store",
      "Content-Security-Policy":`default-src 'none'; script-src 'self'; frame-ancestors ${self.location.origin}; base-uri 'none'; object-src 'none'`,
    }})); return;
  }
  const initiatingClient = event.clientId || event.replacesClientId;
  // Firefox omits the replaced navigation client. The nonce delivered only over
  // the attested owner's private port still binds this navigation to that frame.
  if (parts.length !== 3 || parts[1] !== "file" || !entry.frame || parts[2] !== entry.nonce ||
      (initiatingClient && initiatingClient !== entry.frame) || event.request.referrer !== `${self.location.origin}/downloads/${id}`) {
    event.respondWith(new Response("Download unavailable",{status:404})); return;
  }
  tickets.delete(id); clearTimeout(entry.timer);
  let finish;
  const finished = new Promise(resolve => { finish = resolve; });
  event.waitUntil(finished);
  event.respondWith((async () => {
    if (!await self.clients.get(entry.client)) { entry.port.close(); finish(); return new Response("Download page closed",{status:410}); }
    let pending, closed = false, ownerCheck;
    const close = () => { if (closed) return; closed = true; clearInterval(ownerCheck); entry.port.close(); finish(); };
    const body = new ReadableStream({
      start(controller) {
        ownerCheck = setInterval(async () => {
          if (!closed && !await self.clients.get(entry.client)) {
            controller.error(new Error("Download page closed")); pending?.reject(new Error("Download page closed")); close();
          }
        }, 1000);
        entry.port.onmessage = message => {
          const data = message.data;
          if (closed) return;
          if (data?.kind === "chunk" && pending && data.bytes instanceof ArrayBuffer && data.bytes.byteLength > 0 && data.bytes.byteLength <= 65536) {
            controller.enqueue(new Uint8Array(data.bytes)); const request = pending; pending = undefined; request.resolve();
          } else if (data?.kind === "end" && pending) {
            controller.close(); entry.port.postMessage({kind:"complete"}); pending.resolve(); pending = undefined; close();
          } else {
            controller.error(new Error(typeof data?.error === "string" ? data.error : "Invalid download stream"));
            if (data?.kind === "abort") entry.port.postMessage({kind:"aborted"});
            pending?.reject(new Error(typeof data?.error === "string" ? data.error : "Invalid download stream")); pending = undefined; close();
          }
        };
        entry.port.onmessageerror = () => { controller.error(new Error("Download channel failed")); pending?.reject(new Error("Download channel failed")); close(); };
      },
      pull() { return new Promise((resolve,reject) => { pending = {resolve,reject}; entry.port.postMessage({kind:"pull"}); }); },
      cancel() { entry.port.postMessage({kind:"cancel"}); pending?.resolve(); close(); },
    }, { highWaterMark: 0 });
    return new Response(body, { headers: {
      "Content-Type": "application/octet-stream",
      "Content-Disposition": `attachment; filename*=UTF-8''${encodeURIComponent(entry.filename).replace(/['()*]/g,c=>`%${c.charCodeAt(0).toString(16)}`)}`,
      "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff",
    }});
  })());
});
