pub fn run(arguments: &[String]) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    crate::require_repository_root(&root)?;
    let status = std::process::Command::new("python3")
        .arg(root.join("apps/devtools/distribution.py"))
        .args(arguments)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("paired distribution failed: {status}"))
    }
}
