use super::{Diagnostic, display_path, fenced_blocks, markdown_links, resolve_link_target};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(super) const LOCALE: &str = "zh-CN";

pub(super) fn translation_root(repository: &Path) -> PathBuf {
    repository.join("docs/i18n").join(LOCALE).join("user")
}

pub(super) fn validate(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    validate_levels(
        repository,
        &repository.join("docs/i18n"),
        &[LOCALE, "user"],
        diagnostics,
    );
    validate_readmes(repository, diagnostics);
}

fn entries(
    repository: &Path,
    directory: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<fs::DirEntry> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                repository,
                directory,
                1,
                format!("could not read directory: {error}"),
            ));
            return Vec::new();
        }
    };
    let mut result = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => result.push(entry),
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    directory,
                    1,
                    format!("could not read directory entry: {error}"),
                ));
            }
        }
    }
    result.sort_by_key(fs::DirEntry::file_name);
    result
}

fn validate_levels(
    repository: &Path,
    directory: &Path,
    levels: &[&str],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((expected, remaining)) = levels.split_first() else {
        return;
    };
    for entry in entries(repository, directory, diagnostics) {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && entry.file_name() == *expected => {
                validate_levels(repository, &entry.path(), remaining, diagnostics);
            }
            _ => diagnostics.push(Diagnostic::new(
                repository,
                &entry.path(),
                1,
                format!("unsupported translation location; expected directory `{expected}`"),
            )),
        }
    }
}

pub(super) fn user_paths(
    repository: &Path,
    directory: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if fs::symlink_metadata(directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        diagnostics.push(Diagnostic::new(
            repository,
            directory,
            1,
            "tutorial directories must not be symbolic links",
        ));
        return result;
    }
    let strict = directory.starts_with(repository.join("docs/i18n"));
    for entry in entries(repository, directory, diagnostics) {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => result.extend(user_paths(repository, &path, diagnostics)),
            Ok(kind)
                if kind.is_file()
                    && path.extension().is_some_and(|extension| extension == "md") =>
            {
                result.push(path);
            }
            Ok(kind) if kind.is_symlink() => diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                "tutorial symbolic links are forbidden",
            )),
            Ok(_) if strict => diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                "translation tree requires ordinary Markdown files and directories",
            )),
            Err(error) => diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                format!("could not inspect tutorial entry: {error}"),
            )),
            _ => {}
        }
    }
    result
}

pub(super) fn localized_links(
    repository: &Path,
    path: &Path,
    paired: &Path,
    contents: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let english = repository.join("docs/user");
    for link in markdown_links(contents)
        .into_iter()
        .filter(|link| !link.is_image)
    {
        if let Ok((target, _)) = resolve_link_target(repository, path, &link.destination)
            && target != paired
            && let Ok(relative) = target.strip_prefix(&english)
        {
            diagnostics.push(Diagnostic::new(
                repository,
                path,
                link.line,
                format!(
                    "translated tutorial link must stay in its locale: {}",
                    display_path(repository, &translation_root(repository).join(relative))
                ),
            ));
        }
    }
}

fn validate_readmes(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let english = repository.join("README.md");
    let translated = repository.join(format!("README.{LOCALE}.md"));
    for entry in entries(repository, repository, diagnostics) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("README.")
            && name.ends_with(".md")
            && entry.path() != english
            && entry.path() != translated
        {
            diagnostics.push(Diagnostic::new(
                repository,
                &entry.path(),
                1,
                "unsupported README translation locale",
            ));
        }
    }
    if !english.exists() && !translated.exists() {
        return;
    }
    let mut commands = Vec::new();
    for (source, paired) in [(&english, &translated), (&translated, &english)] {
        let contents = match fs::symlink_metadata(source) {
            Ok(metadata) if metadata.is_file() => fs::read_to_string(source),
            _ => Err(std::io::Error::other(
                "README translation requires an ordinary Markdown file",
            )),
        };
        let Ok(contents) = contents else {
            diagnostics.push(Diagnostic::new(
                repository,
                source,
                1,
                "README translation requires an ordinary Markdown pair",
            ));
            continue;
        };
        validate_commands(repository, source, &contents, diagnostics);
        if !markdown_links(&contents).iter().any(|link| {
            !link.is_image
                && resolve_link_target(repository, source, &link.destination)
                    .is_ok_and(|(target, _)| target == *paired)
        }) {
            diagnostics.push(Diagnostic::new(
                repository,
                source,
                1,
                "README must link to its language counterpart",
            ));
        }
        commands.push(
            fenced_blocks(&contents, &mut BTreeMap::new())
                .into_values()
                .filter(|(language, _, _)| {
                    matches!(language.as_str(), "bash" | "sh" | "powershell")
                })
                .collect::<Vec<_>>(),
        );
    }
    if commands.len() == 2 && commands[0] != commands[1] {
        diagnostics.push(Diagnostic::new(
            repository,
            &translated,
            1,
            "README translated command examples differ",
        ));
    }
}

fn validate_commands(
    repository: &Path,
    path: &Path,
    contents: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let count = fenced_blocks(contents, &mut BTreeMap::new())
        .into_values()
        .filter(|(language, _, _)| matches!(language.as_str(), "bash" | "sh" | "powershell"))
        .count();
    if count == 0 {
        return;
    }
    let original = super::examples(contents, &mut |line, message| {
        diagnostics.push(Diagnostic::new(repository, path, line, message));
    });
    if original.commands.len() != count {
        diagnostics.push(Diagnostic::new(
            repository,
            path,
            1,
            "README shell examples require source-install IDs and platform labels",
        ));
    }
    let owner = repository.join("docs/user/source-install.md");
    let tutorial = match fs::read_to_string(&owner) {
        Ok(contents) => super::examples(&contents, &mut |_, _| {}),
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                repository,
                &owner,
                1,
                format!("could not read README command source: {error}"),
            ));
            return;
        }
    };
    for (id, command) in original.commands {
        if tutorial.commands.get(&id) != Some(&command) {
            diagnostics.push(Diagnostic::new(
                repository,
                path,
                original.identities[&id],
                format!("README command differs from source-install for id `{id}`"),
            ));
        }
    }
}
