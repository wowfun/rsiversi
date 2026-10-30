// One connection owns canonical URLs, exact-ticket reads and bounded waiting interest.
export class ImagePreviews {
  #call;
  #limits;
  #urls;
  #cache = new Map();
  #flights = new Map();
  #queue = [];
  #subscriptions = new Set();
  #active = 0;
  #reserved = 0;
  #cached = 0;
  #converting = 0;
  #closed = false;

  constructor(call, limits, urls = URL) {
    this.#call = call;
    this.#limits = limits;
    this.#urls = urls;
  }

  get snapshot() {
    return { active: this.#active, queued: this.#queue.length, objects: this.#cache.size,
      cached: this.#cached, reserved: this.#reserved, converting: this.#converting,
      keys: [...this.#cache.keys()] };
  }

  acquire(media, ticket) {
    let resolve, reject;
    const result = new Promise((yes, no) => { resolve = yes; reject = no; });
    const subscription = { resolve, reject, live: true, entry: undefined, flight: undefined };
    const release = () => {
      if (!subscription.live) return;
      subscription.live = false;
      this.#subscriptions.delete(subscription);
      if (subscription.entry) subscription.entry.refs--;
      const flight = subscription.flight;
      if (flight) {
        flight.subscribers.delete(subscription);
        if (!flight.started && !flight.subscribers.size) this.#remove(flight);
      }
      resolve(undefined);
      this.#pump();
    };
    const handle = { result, release };
    if (this.#closed) { reject(new Error('The image connection is closed')); return handle; }
    if (!Number.isSafeInteger(media.bytes) || media.bytes < 1 || media.bytes > this.#limits.preview_bytes) {
      reject(new Error('Image exceeds the preview budget')); return handle;
    }
    this.#subscriptions.add(subscription);
    const key = JSON.stringify(media);
    const cached = this.#cache.get(key);
    if (cached) { this.#deliver(subscription, key, cached); return handle; }
    const identity = JSON.stringify([key, ticket]);
    let flight = this.#flights.get(identity);
    if (!flight) {
      if (this.#queue.length >= this.#limits.preview_objects) {
        this.#subscriptions.delete(subscription);
        reject(new Error('Image preview queue is full; retry after another preview finishes'));
        return handle;
      }
      flight = { identity, key, media, ticket, subscribers: new Set(), started: false };
      this.#flights.set(identity, flight);
      this.#queue.push(flight);
    }
    subscription.flight = flight;
    flight.subscribers.add(subscription);
    this.#pump();
    return handle;
  }

  #deliver(subscription, key, entry) {
    if (!subscription.live) return;
    subscription.flight = undefined;
    subscription.entry = entry;
    entry.refs++;
    this.#cache.delete(key);
    this.#cache.set(key, entry);
    subscription.resolve(entry.url);
  }

  #remove(flight) {
    if (this.#flights.get(flight.identity) === flight) this.#flights.delete(flight.identity);
    const index = this.#queue.indexOf(flight);
    if (index !== -1) this.#queue.splice(index, 1);
  }

  #fail(flight, error) {
    for (const subscription of flight.subscribers) {
      subscription.flight = undefined;
      this.#subscriptions.delete(subscription);
      subscription.reject(error);
    }
    flight.subscribers.clear();
    this.#remove(flight);
  }

  #room(bytes) {
    const fits = () => this.#cache.size + this.#active < this.#limits.preview_objects
      && this.#cached + this.#reserved + bytes <= this.#limits.preview_bytes;
    for (const [key, entry] of this.#cache) {
      if (fits()) return true;
      if (entry.refs) continue;
      this.#cache.delete(key);
      this.#cached -= entry.bytes;
      this.#urls.revokeObjectURL(entry.url);
    }
    return fits();
  }

  #pump() {
    if (this.#closed) return;
    while (this.#active < this.#limits.preview_concurrency && this.#queue.length) {
      const flight = this.#queue[0];
      const cached = this.#cache.get(flight.key);
      if (cached) {
        this.#remove(flight);
        for (const subscription of flight.subscribers) this.#deliver(subscription, flight.key, cached);
        continue;
      }
      if (!this.#room(flight.media.bytes)) {
        // Active reads may still release their reservations. Displayed entries
        // remain pinned; once no read can free space, let the user close/retry.
        if (this.#active) return;
        this.#fail(flight, new Error('Image preview budget is in use; close another preview and retry'));
        continue;
      }
      this.#queue.shift();
      flight.started = true;
      this.#active++;
      this.#reserved += flight.media.bytes;
      void this.#read(flight);
    }
  }

  async #read(flight) {
    let bytes;
    try {
      bytes = await this.#call('read_image', JSON.stringify({ kind: 'source', ticket: flight.ticket }));
      if (this.#closed || !flight.subscribers.size) return;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength !== flight.media.bytes
        || bytes.buffer.byteLength !== bytes.byteLength) throw new Error('Image exceeds the preview budget');
      let entry = this.#cache.get(flight.key);
      if (!entry) {
        // Blob construction can copy the transfer buffer. Charge that temporary
        // overlap separately, then retain only the URL's canonical allocation.
        if (this.#converting + bytes.byteLength > this.#limits.preview_bytes) throw new Error('Image conversion is busy');
        this.#converting += bytes.byteLength;
        try {
          entry = { url: this.#urls.createObjectURL(new Blob([bytes], { type: 'image/png' })), bytes: bytes.byteLength, refs: 0 };
          this.#reserved -= flight.media.bytes;
          flight.resolved = true;
          this.#cache.set(flight.key, entry);
          this.#cached += entry.bytes;
        } finally {
          this.#converting -= bytes.byteLength;
          bytes = undefined;
        }
      }
      // Transfer the reservation before resolving consumers or starting another read.
      if (!flight.resolved) {
        this.#reserved -= flight.media.bytes;
        flight.resolved = true;
      }
      for (const subscription of flight.subscribers) this.#deliver(subscription, flight.key, entry);
    } catch (error) {
      this.#fail(flight, error);
    } finally {
      bytes = undefined;
      if (!flight.resolved) this.#reserved -= flight.media.bytes;
      this.#active--;
      this.#remove(flight);
      this.#pump();
    }
  }

  dispose() {
    if (this.#closed) return;
    this.#closed = true;
    for (const subscription of this.#subscriptions) {
      subscription.live = false;
      subscription.resolve(undefined);
    }
    this.#subscriptions.clear();
    for (const flight of this.#flights.values()) flight.subscribers.clear();
    this.#flights.clear();
    this.#queue.length = 0;
    for (const entry of this.#cache.values()) this.#urls.revokeObjectURL(entry.url);
    this.#cache.clear();
    this.#cached = 0;
  }
}
