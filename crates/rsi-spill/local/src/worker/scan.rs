use super::*;
use rsi_files_native_fs::DirectoryEntries;

pub(super) struct Scan {
    scopes: DirectoryEntries,
    current: Option<Scope>,
    cutoff: Option<SystemTime>,
    startup: bool,
}

struct Scope {
    name: String,
    directory: Directory,
    files: DirectoryEntries,
    purged: bool,
    failed: bool,
}

impl Scan {
    pub(super) fn new(worker: &Worker, now: SystemTime, startup: bool) -> io::Result<Self> {
        Ok(Self {
            scopes: worker.directory.entries()?,
            current: None,
            cutoff: (worker.config.cleanup_period_days != 0)
                .then(|| {
                    now.checked_sub(Duration::from_hours(
                        u64::from(worker.config.cleanup_period_days) * 24,
                    ))
                })
                .flatten(),
            startup,
        })
    }

    /// Advances one directory entry or closes one exhausted cursor.
    pub(super) fn step(&mut self, worker: &Worker) -> bool {
        if let Some(scope) = &mut self.current {
            match scope.files.next() {
                Some(Ok(entry)) => scope.visit(worker, &entry, self.cutoff, self.startup),
                Some(Err(_)) => scope.failed = true,
                None => self.current.take().unwrap().finish(worker),
            }
            return true;
        }
        let Some(entry) = self.scopes.next() else {
            return false;
        };
        let Ok(entry) = entry else { return true };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return true;
        };
        if SpillScope::parse(name.into()).is_err()
            || !entry
                .file_type()
                .is_ok_and(rsi_files_native_fs::EntryKind::is_dir)
        {
            return true;
        }
        self.current = Scope::open(worker, name).ok();
        true
    }
}

impl Scope {
    fn open(worker: &Worker, name: &str) -> io::Result<Self> {
        let directory = native::scope_directory(&worker.directory, name, false)?;
        let purged = match directory.child_kind(Path::new(".purged")) {
            Ok(_) => {
                native::open_file(&directory, Path::new(".purged"))?;
                worker.state.fenced.lock().unwrap().insert(name.into());
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        Ok(Self {
            name: name.into(),
            files: directory.entries()?,
            directory,
            purged,
            failed: false,
        })
    }

    fn visit(
        &mut self,
        worker: &Worker,
        entry: &rsi_files_native_fs::DirectoryEntry,
        cutoff: Option<SystemTime>,
        startup: bool,
    ) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { return };
        if !generated(name) {
            return;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {}
            Err(_) => {
                self.failed = true;
                return;
            }
            Ok(_) => return,
        }
        let partial = name.strip_suffix(".part");
        let remove = if self.purged {
            true
        } else if let Some(object) = partial {
            startup
                && SpillLocator::parse(format!("{}-{object}", self.name))
                    .is_ok_and(|locator| !worker.active.contains_key(&locator))
        } else {
            cutoff.is_some_and(|cutoff| {
                native::open_file(&self.directory, Path::new(name))
                    .and_then(|file| file.metadata()?.modified())
                    .is_ok_and(|modified| modified < cutoff)
            })
        };
        if remove && native::remove(&self.directory, Path::new(name)).is_err() {
            self.failed = true;
        }
    }

    fn finish(self, worker: &Worker) {
        let Self {
            name,
            directory,
            files,
            purged,
            failed,
        } = self;
        drop(files);
        if failed || (purged && native::remove(&directory, Path::new(".purged")).is_err()) {
            return;
        }
        drop(directory);
        // Unknown children and active partials keep their namespace.
        let _ = worker.directory.remove_empty_directory(Path::new(&name));
    }
}
