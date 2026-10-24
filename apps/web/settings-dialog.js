// One Settings lifetime spans loading, editor and failure frames on one connection.
export class SettingsDialog {
  constructor({close, send, failed}) {
    this.close = close; this.send = send; this.failed = failed;
    this.reset();
  }
  reset() { this.owner = undefined; this.revision = undefined; this.dismissed = 0n; }
  reopen(owner) { if (this.owner === owner) this.dismissed = 0n; }
  update(owner, revision) {
    if (owner !== this.owner) { this.reset(); this.owner = owner; }
    if (typeof revision !== "string" || !/^[1-9][0-9]{0,19}$/.test(revision)
      || BigInt(revision) > 18446744073709551615n) { this.revision = undefined; return false; }
    this.revision = revision;
    return BigInt(revision) > this.dismissed;
  }
  dismiss() {
    const owner = this.owner, revision = this.revision;
    if (revision) this.dismissed = BigInt(revision) > this.dismissed ? BigInt(revision) : this.dismissed;
    this.close();
    if (owner && revision) Promise.resolve().then(() => this.send(owner, revision)).catch(error => {
      if (this.owner === owner) this.failed(owner, error);
    });
  }
}
