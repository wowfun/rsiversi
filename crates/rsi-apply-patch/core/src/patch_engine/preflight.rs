#![allow(clippy::result_large_err)] // Failures retain the bounded fuzzy-match prefix.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rustix::fs::Mode;

use super::file_update::derive_update;
use super::parser::ParsedOperation;
use super::{
    EngineFailure, MAXIMUM_PATCH_CONTENT_BYTES_TOTAL, MAXIMUM_PATCH_EFFECTS,
    MAXIMUM_PATCH_FAILURE_CODE_BYTES, MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES,
    MAXIMUM_PATCH_FILE_BYTES, MAXIMUM_PATCH_FUZZY_MATCHES, MAXIMUM_PATCH_PATH_BYTES,
    MAXIMUM_PATCH_RESPONSE_BYTES, PatchEffect, PatchEffectKind, PatchFailure, PatchFuzzyMatch,
    PatchHelperResponse, PatchStatus, PreparedKind, PreparedOperation, Root, SafePath,
    VirtualContent,
};

type PreflightFailure = (EngineFailure, Vec<PatchFuzzyMatch>);
type PreflightResult<T> = Result<T, PreflightFailure>;

pub(super) fn preflight(
    root: &Root,
    operations: Vec<ParsedOperation>,
    new_file_mode: Mode,
) -> Result<(Vec<PreparedOperation>, Vec<PatchFuzzyMatch>), PreflightFailure> {
    Preflight::new(root, operations.len(), new_file_mode).run(operations)
}

struct Preflight<'a> {
    root: &'a Root,
    new_file_mode: Mode,
    virtual_files: HashMap<SafePath, Option<VirtualContent>>,
    prepared: Vec<PreparedOperation>,
    fuzzy_matches: Vec<PatchFuzzyMatch>,
    content_bytes: usize,
}

impl<'a> Preflight<'a> {
    fn new(root: &'a Root, operation_count: usize, new_file_mode: Mode) -> Self {
        Self {
            root,
            new_file_mode,
            virtual_files: HashMap::new(),
            prepared: Vec::with_capacity(operation_count),
            fuzzy_matches: Vec::new(),
            content_bytes: 0,
        }
    }

    fn run(
        mut self,
        operations: Vec<ParsedOperation>,
    ) -> Result<(Vec<PreparedOperation>, Vec<PatchFuzzyMatch>), PreflightFailure> {
        for (index, operation) in operations.into_iter().enumerate() {
            let prepared = self.prepare_operation(index, operation)?;
            self.prepared.push(prepared);
        }
        validate_effect_budget(&self.prepared)
            .map_err(|failure| (failure, self.fuzzy_matches.clone()))?;
        validate_response_budget(&self.prepared, &self.fuzzy_matches)
            .map_err(|failure| (failure, Vec::new()))?;
        Ok((self.prepared, self.fuzzy_matches))
    }

    fn prepare_operation(
        &mut self,
        index: usize,
        operation: ParsedOperation,
    ) -> PreflightResult<PreparedOperation> {
        match operation {
            ParsedOperation::Add { path, content } => self.prepare_add(index, path, content),
            ParsedOperation::Delete { path } => self.prepare_delete(index, path),
            ParsedOperation::Update {
                path,
                move_path,
                chunks,
            } => self.prepare_update(index, path, move_path, &chunks),
        }
    }

    fn prepare_add(
        &mut self,
        index: usize,
        path: SafePath,
        content: Vec<u8>,
    ) -> PreflightResult<PreparedOperation> {
        if self.virtual_content(&path, index)?.is_some() {
            return Err(self.at(
                EngineFailure::new("already_exists", "add target already exists"),
                index,
                &path,
            ));
        }
        self.account_content(content.len(), index, &path)?;
        let (parent, prospective_directories) = self
            .root
            .parent_plan(&path)
            .map_err(|failure| self.at(failure, index, &path))?;
        self.virtual_files.insert(
            path.clone(),
            Some(VirtualContent {
                bytes: content.clone(),
                mode: self.new_file_mode,
                identity: None,
            }),
        );
        Ok(PreparedOperation {
            index,
            path,
            expected: None,
            expected_mode: None,
            expected_identity: None,
            parent,
            publish_mode: Some(self.new_file_mode),
            content: Some(content),
            kind: PreparedKind::Add,
            move_path: None,
            prospective_directories,
        })
    }

    fn prepare_delete(
        &mut self,
        index: usize,
        path: SafePath,
    ) -> PreflightResult<PreparedOperation> {
        let expected = self.required_content(&path, index, "delete target does not exist")?;
        self.account_content(expected.bytes.len(), index, &path)?;
        let parent = self
            .root
            .parent_identity(&path)
            .map_err(|failure| self.at(failure, index, &path))?;
        self.virtual_files.insert(path.clone(), None);
        Ok(PreparedOperation {
            index,
            path,
            expected: Some(expected.bytes),
            expected_mode: Some(expected.mode),
            expected_identity: expected.identity,
            parent,
            publish_mode: None,
            content: None,
            kind: PreparedKind::Delete,
            move_path: None,
            prospective_directories: Vec::new(),
        })
    }

    fn prepare_update(
        &mut self,
        index: usize,
        path: SafePath,
        move_path: Option<SafePath>,
        chunks: &[super::parser::UpdateChunk],
    ) -> PreflightResult<PreparedOperation> {
        let expected = self.required_content(&path, index, "update target does not exist")?;
        self.account_content(expected.bytes.len(), index, &path)?;
        let text = std::str::from_utf8(&expected.bytes).map_err(|_| {
            self.at(
                EngineFailure::new("non_utf8_file", "update target is not UTF-8"),
                index,
                &path,
            )
        })?;
        let content = derive_update(text, &path, chunks, index, &mut self.fuzzy_matches)
            .map_err(|failure| (failure, self.fuzzy_matches.clone()))?
            .into_bytes();
        self.account_content(content.len(), index, &path)?;
        let parent = self
            .root
            .parent_identity(&path)
            .map_err(|failure| self.at(failure, index, &path))?;
        let (kind, prospective_directories) =
            self.prepare_update_target(index, &path, move_path.as_ref(), &content, expected.mode)?;
        Ok(PreparedOperation {
            index,
            path,
            expected: Some(expected.bytes),
            expected_mode: Some(expected.mode),
            expected_identity: expected.identity,
            parent,
            publish_mode: Some(expected.mode),
            content: Some(content),
            kind,
            move_path,
            prospective_directories,
        })
    }

    fn prepare_update_target(
        &mut self,
        index: usize,
        path: &SafePath,
        destination: Option<&SafePath>,
        content: &[u8],
        mode: Mode,
    ) -> PreflightResult<(PreparedKind, Vec<String>)> {
        let Some(destination) = destination else {
            self.virtual_files.insert(
                path.clone(),
                Some(VirtualContent {
                    bytes: content.to_vec(),
                    mode,
                    identity: None,
                }),
            );
            return Ok((PreparedKind::Update, Vec::new()));
        };
        if destination == path {
            return Err(self.at(
                EngineFailure::new(
                    "invalid_move",
                    "move destination must differ from the source",
                ),
                index,
                path,
            ));
        }
        if self.virtual_content(destination, index)?.is_some() {
            return Err(self.at(
                EngineFailure::new("already_exists", "move destination already exists"),
                index,
                destination,
            ));
        }
        let (destination_parent, prospective_directories) = self
            .root
            .parent_plan(destination)
            .map_err(|failure| self.at(failure, index, destination))?;
        self.virtual_files.insert(path.clone(), None);
        self.virtual_files.insert(
            destination.clone(),
            Some(VirtualContent {
                bytes: content.to_vec(),
                mode,
                identity: None,
            }),
        );
        Ok((
            PreparedKind::Move { destination_parent },
            prospective_directories,
        ))
    }

    fn required_content(
        &mut self,
        path: &SafePath,
        index: usize,
        missing_message: &str,
    ) -> PreflightResult<VirtualContent> {
        self.virtual_content(path, index)?.ok_or_else(|| {
            self.at(
                EngineFailure::new("not_found", missing_message),
                index,
                path,
            )
        })
    }

    fn virtual_content(
        &mut self,
        path: &SafePath,
        index: usize,
    ) -> PreflightResult<Option<VirtualContent>> {
        if let Some(content) = self.virtual_files.get(path) {
            return Ok(content.clone());
        }
        let content = self
            .root
            .read_optional(path)
            .map_err(|failure| self.at(failure, index, path))?;
        self.virtual_files.insert(path.clone(), content.clone());
        Ok(content)
    }

    fn account_content(
        &mut self,
        bytes: usize,
        index: usize,
        path: &SafePath,
    ) -> PreflightResult<()> {
        account_content(&mut self.content_bytes, bytes)
            .map_err(|failure| self.at(failure, index, path))
    }

    fn at(&self, failure: EngineFailure, index: usize, path: &SafePath) -> PreflightFailure {
        (failure.at(index, path), self.fuzzy_matches.clone())
    }
}

fn validate_effect_budget(prepared: &[PreparedOperation]) -> Result<(), EngineFailure> {
    let mut effects = 0_usize;
    let mut potential_directories = BTreeSet::new();
    for operation in prepared {
        effects = effects
            .checked_add(if matches!(operation.kind, PreparedKind::Move { .. }) {
                2
            } else {
                1
            })
            .ok_or_else(|| EngineFailure::new("effect_budget", "patch effect budget overflow"))?;
        if effects > MAXIMUM_PATCH_EFFECTS {
            return Err(effect_budget_failure());
        }
        for directory in &operation.prospective_directories {
            if !potential_directories.contains(directory) {
                if effects >= MAXIMUM_PATCH_EFFECTS {
                    return Err(effect_budget_failure());
                }
                potential_directories.insert(directory.clone());
                effects += 1;
            }
        }
    }
    Ok(())
}

fn effect_budget_failure() -> EngineFailure {
    EngineFailure::new(
        "effect_budget",
        format!("patch may produce more than {MAXIMUM_PATCH_EFFECTS} filesystem effects"),
    )
}

fn validate_response_budget(
    prepared: &[PreparedOperation],
    fuzzy_matches: &[PatchFuzzyMatch],
) -> Result<(), EngineFailure> {
    if fuzzy_matches.len() > MAXIMUM_PATCH_FUZZY_MATCHES {
        return Err(EngineFailure::new(
            "response_budget",
            format!("patch may report more than {MAXIMUM_PATCH_FUZZY_MATCHES} fuzzy matches"),
        ));
    }
    let effects = projected_effects(prepared)?;
    let response = PatchHelperResponse {
        status: PatchStatus::Partial,
        delta_exact: true,
        effects,
        fuzzy_matches: fuzzy_matches.to_vec(),
        failure: Some(PatchFailure {
            operation: Some(usize::MAX),
            hunk: Some(usize::MAX),
            code: "\0".repeat(MAXIMUM_PATCH_FAILURE_CODE_BYTES),
            message: "\0".repeat(MAXIMUM_PATCH_FAILURE_MESSAGE_BYTES),
            path: Some("\0".repeat(MAXIMUM_PATCH_PATH_BYTES)),
        }),
    };
    if !response.fits_capture() {
        return Err(EngineFailure::new(
            "response_budget",
            format!(
                "patch effect metadata cannot fit the {MAXIMUM_PATCH_RESPONSE_BYTES}-byte helper response"
            ),
        ));
    }
    Ok(())
}

fn projected_effects(prepared: &[PreparedOperation]) -> Result<Vec<PatchEffect>, EngineFailure> {
    let mut directories = BTreeMap::new();
    let mut effects = Vec::new();
    for operation in prepared {
        for directory in &operation.prospective_directories {
            directories
                .entry(directory.clone())
                .or_insert(operation.index);
        }
        let effect = |kind: PatchEffectKind,
                      path: &SafePath,
                      bytes_before: Option<usize>,
                      bytes_after: Option<usize>| PatchEffect {
            operation: operation.index,
            kind,
            path: path.text.clone(),
            bytes_before,
            bytes_after,
        };
        match &operation.kind {
            PreparedKind::Add => effects.push(effect(
                PatchEffectKind::Add,
                &operation.path,
                None,
                operation.content.as_ref().map(Vec::len),
            )),
            PreparedKind::Update => effects.push(effect(
                PatchEffectKind::Update,
                &operation.path,
                operation.expected.as_ref().map(Vec::len),
                operation.content.as_ref().map(Vec::len),
            )),
            PreparedKind::Move { .. } => {
                effects.push(effect(
                    PatchEffectKind::MoveWrite,
                    operation.move_path.as_ref().expect("move has destination"),
                    None,
                    operation.content.as_ref().map(Vec::len),
                ));
                effects.push(effect(
                    PatchEffectKind::MoveDelete,
                    &operation.path,
                    operation.expected.as_ref().map(Vec::len),
                    None,
                ));
            }
            PreparedKind::Delete => effects.push(effect(
                PatchEffectKind::Delete,
                &operation.path,
                operation.expected.as_ref().map(Vec::len),
                None,
            )),
        }
    }
    effects.extend(
        directories
            .into_iter()
            .map(|(path, operation)| PatchEffect {
                operation,
                kind: PatchEffectKind::Mkdir,
                path,
                bytes_before: None,
                bytes_after: None,
            }),
    );
    if effects.len() > MAXIMUM_PATCH_EFFECTS {
        return Err(EngineFailure::new(
            "effect_budget",
            format!("patch may produce more than {MAXIMUM_PATCH_EFFECTS} filesystem effects"),
        ));
    }
    Ok(effects)
}

fn account_content(total: &mut usize, bytes: usize) -> Result<(), EngineFailure> {
    if bytes > MAXIMUM_PATCH_FILE_BYTES {
        return Err(EngineFailure::new(
            "file_too_large",
            format!("file exceeds {MAXIMUM_PATCH_FILE_BYTES} bytes"),
        ));
    }
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| EngineFailure::new("content_budget", "content budget overflow"))?;
    if *total > MAXIMUM_PATCH_CONTENT_BYTES_TOTAL {
        return Err(EngineFailure::new(
            "content_budget",
            format!("patch content exceeds {MAXIMUM_PATCH_CONTENT_BYTES_TOTAL} aggregate bytes"),
        ));
    }
    Ok(())
}
