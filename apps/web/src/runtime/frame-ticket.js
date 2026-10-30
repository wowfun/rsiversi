// One exact presentation lifetime; a late commit cannot settle its replacement.
export class FrameTicket {
  #resolveCommit;
  #rejectCommit;
  #interrupt;
  #timer;
  #failure;
  #committed = false;
  #finished = false;
  #hosts = [];

  constructor(epoch, frameId, failed, deadlineMs = 30_000) {
    this.epoch = epoch;
    this.frameId = frameId;
    this.committed = new Promise((resolve, reject) => {
      this.#resolveCommit = resolve;
      this.#rejectCommit = reject;
    });
    this.interrupted = new Promise((_, reject) => {
      this.#interrupt = reject;
    });
    // Failure may precede the caller's first await (for example a caught React error).
    this.committed.catch(() => {});
    this.interrupted.catch(() => {});
    this.#timer = setTimeout(() => {
      const error = new Error(
        "Presentation did not complete within 30 seconds",
      );
      this.abort(error);
      failed(error);
    }, deadlineMs);
  }

  requireHosts(hosts) {
    this.#hosts = hosts;
  }
  check() {
    if (this.#failure) throw this.#failure;
    if (
      (typeof this.#hosts === "function" ? this.#hosts() : this.#hosts).some(
        (host) => !host?.isConnected,
      )
    )
      throw new Error("A required presentation host is detached");
  }
  commit(epoch, frameId) {
    if (
      this.#finished ||
      this.#failure ||
      epoch !== this.epoch ||
      frameId !== this.frameId
    )
      return false;
    try {
      this.check();
    } catch (error) {
      this.abort(error);
      return false;
    }
    this.#committed = true;
    this.#resolveCommit();
    return true;
  }
  wait(operation) {
    return Promise.race([operation, this.interrupted]);
  }
  finish() {
    this.check();
    if (!this.#committed)
      throw new Error("React has not committed this presentation");
    this.#finished = true;
    clearTimeout(this.#timer);
  }
  abort(error) {
    if (this.#finished || this.#failure) return;
    this.#failure = error instanceof Error ? error : new Error(String(error));
    clearTimeout(this.#timer);
    this.#rejectCommit(this.#failure);
    this.#interrupt(this.#failure);
  }
}
