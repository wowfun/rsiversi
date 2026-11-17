use super::{Directory, Path, SpillLocator, SpillScope, Worker, discard, generated, io, native};
use rsi_files_native_fs::DirectoryEntries;

pub(super) struct Cleanup {
    scope: SpillScope,
    directory: Directory,
    entries: DirectoryEntries,
    pub(super) remove: bool,
    clean: bool,
    failure: Option<io::Error>,
}

impl Cleanup {
    pub(super) fn open(
        worker: &Worker,
        scope: &SpillScope,
        remove: bool,
    ) -> io::Result<Option<Self>> {
        if remove {
            worker
                .state
                .fenced
                .lock()
                .unwrap()
                .insert(scope.as_str().into());
        }
        let directory = match native::scope_directory(&worker.directory, scope.as_str(), false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if remove {
            match native::create_file(&directory, Path::new(".purged")) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    native::open_file(&directory, Path::new(".purged"))?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(Self {
            scope: scope.clone(),
            entries: directory.entries()?,
            directory,
            remove,
            clean: true,
            failure: None,
        }))
    }

    pub(super) fn removing(&self, scope: &SpillScope) -> bool {
        self.remove && &self.scope == scope
    }

    pub(super) fn step(&mut self, worker: &mut Worker) -> bool {
        let Some(entry) = self.entries.next() else {
            return false;
        };
        let result = (|| {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".purged" {
                if !self.remove {
                    self.clean = false;
                }
                return Ok(());
            }
            if !generated(&name.to_string_lossy()) || !entry.file_type()?.is_file() {
                return Ok(());
            }
            if !self.remove {
                self.clean = false;
                return Ok(());
            }
            if let Some(object) = name.to_str().and_then(|name| name.strip_suffix(".part")) {
                let locator = SpillLocator::parse(format!("{}-{object}", self.scope.as_str()))
                    .expect("generated scope and object");
                if let Some(active) = worker.active.remove(&locator) {
                    discard(&locator, active);
                }
            }
            native::remove(&self.directory, Path::new(&name))
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
        }
        self.clean
    }

    pub(super) fn finish(self, worker: &Worker) -> io::Result<bool> {
        let Self {
            scope,
            directory,
            entries,
            remove,
            clean,
            failure,
        } = self;
        drop(entries);
        if let Some(error) = failure {
            return Err(error);
        }
        if remove {
            native::remove(&directory, Path::new(".purged"))?;
            drop(directory);
            match worker
                .directory
                .remove_empty_directory(Path::new(scope.as_str()))
            {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(clean)
    }
}
