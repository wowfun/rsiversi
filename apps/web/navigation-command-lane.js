// Presentation scheduling only. Rust still owns tickets, authorization and effect settlement.
export class NavigationCommandLane {
  #tail = Promise.resolve(); #pending = 0;
  /** @template T @param {() => Promise<T> | T} work @returns {Promise<T>} */
  run(work) {
    if (this.#pending >= 32) return Promise.reject(new Error('Navigation command capacity is full.'));
    this.#pending++;
    const result = this.#tail.then(work);
    this.#tail = result.then(() => {}, () => {}).finally(() => { this.#pending--; });
    return result;
  }
}
