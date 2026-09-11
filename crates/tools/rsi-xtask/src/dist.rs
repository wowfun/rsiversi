pub fn run(arguments: &[String]) -> Result<(), String> {
    if !matches!(arguments, [kind, _] if kind == "desktop")
        && !matches!(arguments, [kind, _, debug] if kind == "desktop" && debug == "--debug")
    {
        return Err("usage: cargo xtask dist desktop /absolute/output [--debug]".into());
    }
    let root = std::env::current_dir().map_err(|error| error.to_string())?;
    crate::require_repository_root(&root)?;
    let status = std::process::Command::new("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/dist-desktop.py"))
        .args(&arguments[1..])
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("desktop distribution failed: {status}"))
    }
}
