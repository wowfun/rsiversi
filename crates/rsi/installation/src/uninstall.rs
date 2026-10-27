//! Offline removal uses exact installation roles and explicit home-data admission.
use crate::{
    Result, error, filesystem as fs,
    record::{Layout, Phase, Record, Role},
};
use rsi_files_native_fs::{self as native, Directory, EntryKind};
use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, HostStartupLease, ServiceHostPaths};
use std::{
    collections::BTreeSet,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent command-line authorizations, not mutually exclusive states.
pub struct Options {
    pub prefix: PathBuf,
    pub homes: HostPaths,
    pub purge: bool,
    pub dry_run: bool,
    pub no_path: bool,
    pub force: bool,
    pub yes: bool,
}
const EXTRA_STATE: &[&str] = &[
    "desktop-webview",
    "trust",
    "session-host/artifacts",
    "session-host/owner.log",
    "session-host/owner.json",
    "session-host/runtime",
];
const CACHE: &[&str] = &[
    "history",
    "process-output",
    "workspace-review",
    "native-applications",
    "native-addons",
    "agent-presets/system",
];
const PREVIEW_ENTRIES: usize = 1024;
const ANCHORS: &[&str] = &[".rsi-namespace-pin", fs::LOCK, "owner.lock", "startup.lock"];

fn confirmation(message: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(error(format!(
            "{message}; noninteractive operation requires --yes"
        )));
    }
    eprint!("{message} [y/N] ");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !matches!(input.trim(), "y" | "Y" | "yes" | "YES") {
        return Err(error("cancelled; nothing removed"));
    }
    Ok(())
}
fn record(layout: &Layout, force: bool) -> Result<Option<Record>> {
    match layout.record() {
        Ok(record) => Ok(record),
        Err(e) if force => {
            eprintln!("Installation record unavailable: {e}; PATH ownership will not be inferred.");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
fn pending(homes: &HostPaths) -> Result<()> {
    for root in [homes.config(), homes.state()] {
        for name in [
            "reset.pending.json",
            "import.pending.json",
            "import.staging.json",
        ] {
            match std::fs::symlink_metadata(root.join("session-host").join(name)) {
                Ok(_) => {
                    return Err(error(format!(
                        "pending {name}; recover maintenance with its original homes first"
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
fn data_plan(homes: &HostPaths) -> Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>)> {
    let mut targets = BTreeSet::new();
    targets.insert(homes.config().join("credentials"));
    for name in crate::receipts::STATE.iter().chain(EXTRA_STATE) {
        targets.insert(homes.state().join(name));
    }
    for name in CACHE {
        targets.insert(homes.cache().join(name));
    }
    let (backups, _, receipts) = crate::receipts::backups(homes)?;
    targets.extend(backups);
    Ok((targets, receipts))
}
fn visit(
    parent: &Directory,
    name: &Path,
    display: &Path,
    depth: usize,
    budget: &mut usize,
    remove: bool,
) -> Result<(u64, bool)> {
    if depth > 64 {
        return Err(error(format!(
            "tree exceeds depth 64 at {}; move nested data out of this managed tree before retrying",
            display.display()
        )));
    }
    if *budget == 0 {
        if remove {
            return Err(error(
                "removal pass reached 1,048,576 entries after making progress; retry to continue",
            ));
        }
        return Ok((0, false));
    }
    *budget -= 1;
    let kind = match parent.child_kind(name) {
        Ok(kind) => kind,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, true)),
        Err(e) => return Err(e),
    };
    match kind {
        EntryKind::File => {
            let size = native::open_relative_file_no_follow(parent, name)?
                .metadata()?
                .len();
            if remove {
                parent.remove_child(name)?;
            }
            Ok((size, true))
        }
        EntryKind::Link => {
            if remove {
                parent.remove_link(name)?;
            }
            Ok((0, true))
        }
        EntryKind::Directory => {
            let directory = native::open_relative_directory_no_follow(parent, name)?;
            let mut total = 0_u64;
            let mut empty = true;
            for entry in directory.entries()? {
                let entry = entry?;
                let name = entry.file_name();
                if name.to_str().is_some_and(|s| ANCHORS.contains(&s)) {
                    empty = false;
                    continue;
                }
                if *budget == 0 && !remove {
                    empty = false;
                    break;
                }
                let (bytes, removed) = visit(
                    &directory,
                    Path::new(&name),
                    &display.join(&name),
                    depth + 1,
                    budget,
                    remove,
                )?;
                total = total
                    .checked_add(bytes)
                    .ok_or_else(|| error("size overflow"))?;
                empty &= removed;
            }
            drop(directory);
            if remove && empty {
                parent.remove_empty_directory(name)?;
            }
            Ok((total, empty))
        }
        EntryKind::Other => {
            let message = format!(
                "special file retained: {}; stop its owning process and remove this entry manually before retrying",
                display.display()
            );
            if remove {
                Err(error(message))
            } else {
                eprintln!("{message}");
                Ok((0, false))
            }
        }
    }
}
fn tree(path: &Path, remove: bool) -> Result<(u64, bool)> {
    let parent = match native::open_absolute_directory_no_follow(
        path.parent()
            .ok_or_else(|| error("missing deletion parent"))?,
    ) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, true)),
        Err(e) => return Err(e),
    };
    visit(
        &parent,
        Path::new(
            path.file_name()
                .ok_or_else(|| error("cannot remove a root"))?,
        ),
        path,
        0,
        &mut if remove { 1_048_576 } else { PREVIEW_ENTRIES },
        remove,
    )
    .map_err(|e| error(format!("{}: {e}", path.display())))
}
fn preview_size(path: &Path) -> String {
    match tree(path, false) {
        Ok((bytes, true)) => format!("{bytes} bytes"),
        Ok((bytes, false)) => format!("at least {bytes} bytes; preview incomplete"),
        Err(e) => format!("size unavailable: {e}"),
    }
}
fn retained(
    root: &Path,
    removed: &BTreeSet<PathBuf>,
    packages: Option<&Path>,
    budget: &mut usize,
) -> Result<()> {
    let directory = match native::open_absolute_directory_no_follow(root) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in directory.entries()? {
        if *budget == 0 {
            println!(
                "Keep: remaining entries under {} (preview limit reached)",
                root.display()
            );
            return Ok(());
        }
        *budget -= 1;
        let entry = entry?;
        let name = entry.file_name();
        let path = root.join(name);
        if removed.contains(&path) || packages == Some(path.as_path()) {
            continue;
        }
        if removed.iter().any(|candidate| candidate.starts_with(&path))
            && entry.file_type()? == EntryKind::Directory
        {
            retained(&path, removed, packages, budget)?;
        } else {
            println!("Keep: {}", path.display());
        }
    }
    Ok(())
}
fn installation_present(layout: &Layout) -> Result<bool> {
    let mut found = false;
    for role in Role::ALL {
        match std::fs::symlink_metadata(layout.target(role)) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => found = true,
            Ok(_) => {
                return Err(error(format!(
                    "installation target is a directory or link: {}",
                    layout.target(role).display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}
#[allow(clippy::too_many_lines)] // Keep all retained leases live across revalidation, stop, ordered deletion and retry settlement.
pub async fn uninstall(options: Options) -> Result<()> {
    let layout = Layout::new(&options.prefix)?;
    let present = installation_present(&layout)?;
    layout.reject_pending()?;
    pending(&options.homes)?;
    let mut receipt = record(&layout, options.force)?;
    if present && receipt.is_none() && !options.force {
        return Err(error(
            "no reliable installation record; use a trusted external uninstaller with --force to confirm fixed targets",
        ));
    }
    println!(
        "Installation: {}\nConfig: {}\nState: {}\nCache: {}\nHomes are shared by installations and development builds.",
        layout.prefix.display(),
        options.homes.config().display(),
        options.homes.state().display(),
        options.homes.cache().display()
    );
    for root in [
        options.homes.config(),
        options.homes.state(),
        options.homes.cache(),
    ] {
        fs::absolute(root)?;
        if root.parent().is_none() || root == layout.bin || root == layout.control {
            return Err(error(
                "unsafe product home overlaps installation authority or filesystem root",
            ));
        }
    }
    let (targets, cleanup_receipts) = if options.purge {
        data_plan(&options.homes)?
    } else {
        (BTreeSet::new(), BTreeSet::new())
    };
    for target in targets
        .iter()
        .chain(std::iter::once(&options.homes.state().join("packages")))
    {
        if [
            options.homes.config(),
            options.homes.state(),
            options.homes.cache(),
            &layout.bin,
            &layout.control,
        ]
        .iter()
        .any(|root| root.starts_with(target))
        {
            return Err(error(format!(
                "purge target overlaps protected authority: {}",
                target.display()
            )));
        }
    }
    let upgrade_files = if receipt.is_some() {
        crate::transaction::preview_collection(&layout)?
    } else {
        vec![]
    };
    for path in &upgrade_files {
        println!("Remove completed upgrade file: {}", path.display());
    }
    let mut install_files: BTreeSet<_> = Role::ALL
        .into_iter()
        .map(|role| layout.target(role))
        .chain(upgrade_files)
        .collect();
    if let Some(record) = &receipt {
        for (role, expected) in [
            (Role::Core, &record.core),
            (Role::Uninstaller, &record.uninstaller),
            (Role::Launcher, &record.launcher),
        ] {
            let stage =
                layout
                    .directory(role)
                    .join(format!(".rsi-install-{}-{}", record.id, role.name()));
            match std::fs::symlink_metadata(&stage) {
                Ok(_) => {
                    if fs::digest_file(fs::source(&stage)?)? != *expected {
                        return Err(error(format!(
                            "unverified installation stage retained: {}",
                            stage.display()
                        )));
                    }
                    println!("Remove owned installation stage: {}", stage.display());
                    install_files.insert(stage);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    for directory in [&layout.bin, &layout.control] {
        let mut budget = PREVIEW_ENTRIES;
        retained(directory, &install_files, None, &mut budget)?;
    }
    for role in Role::ALL {
        if layout.target(role).exists() {
            println!(
                "Remove installation file: {}",
                layout.target(role).display()
            );
        }
    }
    for path in targets.iter().chain(&cleanup_receipts) {
        println!("Purge: {} ({})", path.display(), preview_size(path));
    }
    for path in [
        options.homes.state().join("packages"),
        options.homes.state().join("session-host/artifacts"),
    ] {
        if targets.contains(&path) {
            continue;
        }
        let size = preview_size(&path);
        println!(
            "{} shared payload: {} ({size})",
            if options.purge { "Purge" } else { "Keep" },
            path.display()
        );
    }
    for root in [
        options.homes.config(),
        options.homes.state(),
        options.homes.cache(),
    ] {
        let mut budget = PREVIEW_ENTRIES;
        retained(
            root,
            &targets.union(&cleanup_receipts).cloned().collect(),
            options
                .purge
                .then(|| options.homes.state().join("packages"))
                .as_deref(),
            &mut budget,
        )?;
    }
    println!(
        "Keep user-authored configuration, AGENTS.md, skills, agents, Profiles, custom presets, addon sources and unknown files."
    );
    let host = ServiceHostPaths::from_host_paths(&options.homes).map_err(error)?;
    let preview = host.read_lifecycle_identity().map_err(error)?;
    if let Some(owner) = &preview {
        println!(
            "Host PID {} ({:?}); current={}",
            owner.pid,
            owner.mode,
            owner.is_current().map_err(error)?
        );
    }
    if options.purge
        && let Some(path) = preview
            .as_ref()
            .and_then(|owner| owner.socket_path.as_ref())
    {
        println!("Recorded runtime endpoint: {}", path.display());
    }
    if options.dry_run {
        return Ok(());
    }
    if !present && !options.purge {
        println!("No installation files found; homes were not cleaned.");
        return Ok(());
    }
    if options.purge {
        confirmation(
            "Purge these shared homes? Other installations may use their credentials, Sessions and packages",
            options.yes,
        )?;
    }
    if present && receipt.is_none() {
        confirmation(
            "Force-remove the fixed installation files listed above without ownership receipts?",
            options.yes,
        )?;
    }
    let needs_homes =
        options.purge || options.homes.config().exists() || options.homes.state().exists();
    let _startup = if needs_homes {
        Some(HostStartupLease::try_acquire(&host).map_err(error)?)
    } else {
        None
    };
    let bin = fs::Lease::acquire(&layout.bin, true, false)?;
    let control = fs::Lease::acquire(&layout.control, true, false)?;
    // Revalidate after obtaining both directory authorities, before stopping/deleting.
    layout.reject_pending()?;
    pending(&options.homes)?;
    installation_present(&layout)?;
    let locked_record = record(&layout, options.force)?;
    if receipt.as_ref().map(|r| &r.id) != locked_record.as_ref().map(|r| &r.id) {
        return Err(error("installation changed during preflight; retry"));
    }
    let mut stages = Vec::new();
    if let Some(record) = &receipt {
        for (role, expected) in [
            (Role::Core, &record.core),
            (Role::Uninstaller, &record.uninstaller),
            (Role::Launcher, &record.launcher),
        ] {
            let name = format!(".rsi-install-{}-{}", record.id, role.name());
            let dir = if role == Role::Core {
                &bin.directory
            } else {
                &control.directory
            };
            if let Some(hash) = fs::digest(dir, &name)? {
                if &hash != expected {
                    return Err(error(format!(
                        "unverified installation stage retained: {}; inspect before retrying",
                        layout.directory(role).join(&name).display()
                    )));
                }
                stages.push((role, name));
            }
        }
    }
    let observed_owner = host.read_lifecycle_identity().map_err(error)?;
    if let Some(owner) = &observed_owner
        && owner.is_current().map_err(error)?
    {
        let metadata = host.read_metadata().map_err(error)?;
        let origin = metadata.as_ref().and_then(|m| m.installation.as_ref());
        let ours = origin.is_some_and(|o| {
            receipt
                .as_ref()
                .is_some_and(|r| r.id == o.id && r.prefix == o.prefix)
        });
        if options.purge || ours || origin.is_none() {
            if !options.purge && !ours {
                confirmation(
                    &format!(
                        "Host PID {} has unknown installation origin; stop it gracefully?",
                        owner.pid
                    ),
                    options.yes,
                )?;
            }
            rsi_service_host::stop_gracefully(owner)
                .await
                .map_err(error)?;
        } else {
            println!("Keep Host PID {} from another installation.", owner.pid);
        }
    }
    let home_owner = if options.purge {
        Some(HostOwnerLease::try_acquire(host).map_err(error)?)
    } else {
        None
    };
    pending(&options.homes)?;
    let (locked_targets, locked_receipts) = if options.purge {
        data_plan(&options.homes)?
    } else {
        (BTreeSet::new(), BTreeSet::new())
    };
    if locked_targets != targets || locked_receipts != cleanup_receipts {
        return Err(error("cleanup plan changed; preview and retry"));
    }
    let package_root = options.homes.state().join("packages");
    let packages = if options.purge && package_root.exists() {
        Some(
            rsi_packages::PackageStore::open(&package_root)
                .map_err(error)?
                .into_purge()
                .map_err(error)?,
        )
    } else {
        None
    };
    if let Some(record) = &mut receipt {
        record.phase = Phase::Uninstalling;
        record.save(&control.directory, true)?;
    }
    if fs::kind(&bin.directory, Role::Core.name())?.is_some() {
        bin.directory
            .remove_child(Path::new(Role::Core.name()))
            .map_err(|e| {
                error(format!(
                    "cannot remove {}: {e}; close TUI/Desktop clients and retry",
                    layout.target(Role::Core).display()
                ))
            })?;
        println!("Removed {}", layout.target(Role::Core).display());
    }
    if receipt.is_some() {
        crate::transaction::collect(&layout, &bin.directory, &control.directory)?;
    }
    for (role, name) in stages {
        let dir = if role == Role::Core {
            &bin.directory
        } else {
            &control.directory
        };
        dir.remove_child(Path::new(&name))?;
    }
    if options.purge {
        if let (Some(owner), Some(lease)) = (&observed_owner, &home_owner) {
            rsi_service_host::cleanup_stopped_endpoints(owner, lease).map_err(error)?;
        }
        if let Some(packages) = packages {
            packages.purge().map_err(error)?;
        }
        for path in &targets {
            tree(path, true).map_err(|e| {
                error(format!(
                    "partial removal; retained {}: {e}; retry with the same options",
                    path.display()
                ))
            })?;
            println!("Cleaned {}", path.display());
        }
        for path in &cleanup_receipts {
            tree(path, true)?;
            println!("Removed completed receipt {}", path.display());
        }
    }
    if !options.no_path {
        let mut others = false;
        for entry in bin.directory.entries()? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !ANCHORS.contains(&name.as_ref())
                && !(name.starts_with(".rsi-core-upgrade-")
                    && (name.ends_with(".committed.json") || name.ends_with(".recovered.json")))
                && name != Role::Core.name()
            {
                others = true;
                break;
            }
        }
        if others {
            println!("Keep shared bin PATH entry: other programs remain.");
        } else if let Some(record) = &receipt {
            for edit in &record.path_edits {
                if edit.applied {
                    if let Err(error) = crate::path_receipt::apply(edit, true) {
                        eprintln!(
                            "PATH retained for {}: {error}; remove this installation's PATH entry manually",
                            edit.target
                        );
                    }
                } else {
                    println!("Keep uncertain PATH receipt: {}", edit.target);
                }
            }
        } else {
            println!("Keep PATH: no reliable ownership receipt.");
        }
    }
    for entry in bin.directory.entries()? {
        let name = entry?.file_name();
        let text = name.to_string_lossy();
        if receipt.is_some()
            && text.starts_with(".rsi-core-upgrade-")
            && (text.ends_with(".committed.json") || text.ends_with(".recovered.json"))
        {
            bin.directory.remove_child(Path::new(&name))?;
        }
    }
    for role in [Role::Uninstaller, Role::Launcher, Role::Record] {
        if fs::kind(&control.directory, role.name())?.is_some() {
            control.directory.remove_child(Path::new(role.name()))?;
        }
    }
    println!(
        "Uninstalled. Empty coordination anchors are retained. {}",
        if options.purge {
            "User-authored files and unknown entries remain."
        } else {
            "Homes were preserved; later purge requires an external prebuilt --uninstaller."
        }
    );
    Ok(())
}
