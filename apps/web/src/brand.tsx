export function Brand({compact=false}: {compact?:boolean}) {
  const logo=<img className="brand-logo" src="/logo.svg" width={32} height={32} alt={compact?'RSIversi':''}/>
  return compact
    ? <span className="wordmark" title="RSIversi">{logo}</span>
    : <a className="wordmark" href="/" aria-label="RSIversi home">{logo}<span>RSIversi</span></a>
}
