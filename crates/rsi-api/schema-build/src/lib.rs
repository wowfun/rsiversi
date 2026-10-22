//! Deterministic schema snapshots for explicitly versioned Rust wire contracts.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use quote::ToTokens as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use syn::visit_mut::VisitMut as _;

/// Owning protocol identity and semantic revision.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    /// Stable protocol identity.
    pub name: String,
    /// Explicit semantic model revision.
    pub version: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractInput {
    name: String,
    version: u32,
    #[serde(default)]
    sources: Vec<String>,
}
/// Generated normalized source schema.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    /// Generator format revision.
    pub format: u32,
    /// Protocol authority.
    pub contract: Contract,
    /// Shared JSON wire policy.
    pub json: &'static str,
    /// Models and codecs, keyed by owning package and source module.
    pub models: BTreeMap<String, String>,
    /// Resolved external dependency identities, including external model packages.
    pub dependencies: BTreeMap<String, String>,
}
/// Read-only schema generation result and exact observed files.
#[derive(Debug)]
pub struct Generated {
    /// Normalized contract.
    pub snapshot: Snapshot,
    /// Deterministic pretty-printed JSON bytes.
    pub bytes: Vec<u8>,
    /// SHA-256 of those bytes.
    pub digest: String,
    /// Inputs Cargo must observe.
    pub inputs: Vec<PathBuf>,
}
/// Reads source contracts without invoking Cargo or dependent build scripts.
///
/// # Errors
/// Returns an error for unreadable or invalid contracts, inadmissible source
/// paths, exceeded input bounds, or a source model that cannot be parsed.
pub fn generate(root: &Path) -> Result<Generated, String> {
    Generator::default().generate(root)
}

/// Reuses source parsing within one immutable verification invocation.
#[derive(Debug, Default)]
pub struct Generator {
    fragments: BTreeMap<PathBuf, Fragment>,
}
#[derive(Debug)]
struct Fragment {
    cargo: toml::Value,
    inputs: Vec<PathBuf>,
    models: BTreeMap<String, String>,
}
fn valid_identity(contract: &Contract) -> bool {
    !(contract.version == 0
        || contract.name.is_empty()
        || contract.name.len() > 128
        || !contract
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)))
}
impl Generator {
    /// Generates one contract from this invocation's source observation.
    ///
    /// # Errors
    /// Returns the same input and validation errors as [`generate`].
    pub fn generate(&mut self, root: &Path) -> Result<Generated, String> {
        let contract_path = root.join("wire-contract.toml");
        let input: ContractInput =
            toml::from_str(&read(&contract_path)?).map_err(|e| e.to_string())?;
        let contract = Contract {
            name: input.name,
            version: input.version,
        };
        if !valid_identity(&contract) {
            return Err("invalid wire contract identity or version".into());
        }
        let mut models = BTreeMap::new();
        let mut visited = BTreeSet::new();
        let mut inputs = vec![contract_path];
        let workspace = workspace(root)?;
        let workspace_path = workspace
            .root
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if input.sources.len() > 64 {
            return Err("too many additional wire sources".into());
        }
        for source in input.sources {
            let path = root
                .join(&source)
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let relative = path
                .strip_prefix(&workspace_path)
                .map_err(|_| "wire source escapes workspace")?;
            if !path.is_file()
                || std::fs::metadata(&path)
                    .map_err(|error| error.to_string())?
                    .len()
                    > 16 * 1024 * 1024
            {
                return Err("wire source must be a bounded regular file".into());
            }
            let key = format!("source:{}", relative.to_string_lossy().replace('\\', "/"));
            let source = read(&path)?.replace("\r\n", "\n");
            let source = if path.extension().is_some_and(|extension| extension == "rs") {
                let mut syntax = syn::parse_file(&source).map_err(|error| error.to_string())?;
                StripDocs.visit_file_mut(&mut syntax);
                syntax.to_token_stream().to_string()
            } else {
                source
            };
            if models.insert(key, source).is_some() {
                return Err("duplicate additional wire source".into());
            }
            inputs.push(path);
        }
        inputs.extend([
            workspace.root.join("Cargo.toml"),
            workspace.root.join("Cargo.lock"),
        ]);
        let mut dependencies = BTreeMap::new();
        package(
            root,
            &mut visited,
            &mut inputs,
            &mut models,
            &workspace,
            &mut dependencies,
            &mut self.fragments,
        )?;
        let snapshot = Snapshot {
            format: 2,
            contract,
            json: "exact-numbers;ordered-objects;utf8;serde-json-v1",
            models,
            dependencies,
        };
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?;
        let digest = hex::encode(Sha256::digest(&bytes));
        Ok(Generated {
            snapshot,
            bytes,
            digest,
            inputs,
        })
    }
}
/// Cargo build-script entry point. A source schema never depends on build output.
///
/// # Panics
/// Panics when Cargo's required environment is absent, source generation fails,
/// or the generated descriptor cannot be written to Cargo's output directory.
pub fn build() {
    let root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let generated = generate(&root).unwrap_or_else(|e| panic!("wire contract: {e}"));
    let snapshot = root.join("wire-schema.json");
    println!("cargo:rerun-if-changed={}", snapshot.display());
    require_reviewed(&snapshot, &generated.bytes).unwrap_or_else(|error| panic!("{error}"));
    for path in generated.inputs {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rustc-env=RSI_WIRE_NAME={}",
        generated.snapshot.contract.name
    );
    println!(
        "cargo:rustc-env=RSI_WIRE_VERSION={}",
        generated.snapshot.contract.version
    );
    println!("cargo:rustc-env=RSI_WIRE_SCHEMA={}", generated.digest);
}
fn require_reviewed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).ok().as_deref() != Some(bytes) {
        return Err(format!(
            "{}: stale wire snapshot; review source/contract changes, then run cargo xtask verify-protocols --write",
            path.display()
        ));
    }
    Ok(())
}
fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn fragment(root: &Path) -> Result<Fragment, String> {
    let mut fragment_inputs = Vec::new();
    let mut fragment_models = BTreeMap::new();
    let manifest = root.join("Cargo.toml");
    let cargo: toml::Value = toml::from_str(&read(&manifest)?).map_err(|e| e.to_string())?;
    fragment_inputs.push(manifest);
    let name = cargo
        .get("package")
        .and_then(|v| v.get("name"))
        .and_then(toml::Value::as_str)
        .ok_or("missing package name")?;
    let mut files = Vec::new();
    source_files(&root.join("src"), &mut files)?;
    fragment_inputs.extend(files.iter().filter(|path| path.is_dir()).cloned());
    files.retain(|path| path.is_file());
    files.sort();
    let mut parsed_files = Vec::new();
    let mut codec_types = BTreeSet::new();
    for path in files {
        let parsed =
            syn::parse_file(&read(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        collect_codec_types(&parsed.items, &mut codec_types);
        parsed_files.push((relative, parsed.items));
        fragment_inputs.push(path);
    }
    let helpers = reachable_helpers(&parsed_files, &codec_types);
    for (relative, parsed) in parsed_files {
        items(
            &format!("{name}/{relative}"),
            parsed,
            &mut fragment_models,
            &codec_types,
            &helpers,
        );
    }
    if let Some(features) = cargo.get("features") {
        fragment_models.insert(format!("{name}/Cargo.toml/features"), features.to_string());
    }

    Ok(Fragment {
        cargo,
        inputs: fragment_inputs,
        models: fragment_models,
    })
}

fn package(
    root: &Path,
    visited: &mut BTreeSet<PathBuf>,
    inputs: &mut Vec<PathBuf>,
    models: &mut BTreeMap<String, String>,
    workspace: &Workspace,
    dependencies: &mut BTreeMap<String, String>,
    cache: &mut BTreeMap<PathBuf, Fragment>,
) -> Result<(), String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if !root.starts_with(workspace.root.canonicalize().map_err(|e| e.to_string())?) {
        return Err("wire dependency escapes workspace".into());
    }
    if !visited.insert(root.clone()) {
        return Ok(());
    }
    if !cache.contains_key(&root) {
        cache.insert(root.clone(), fragment(&root)?);
    }
    let fragment = cache.get(&root).expect("observed package");
    inputs.extend(fragment.inputs.iter().cloned());
    models.extend(fragment.models.clone());
    let cargo = fragment.cargo.clone();
    let name = cargo["package"]["name"]
        .as_str()
        .expect("validated package name");
    let mut dependency_tables = Vec::new();
    if let Some(table) = cargo.get("dependencies").and_then(toml::Value::as_table) {
        dependency_tables.push(("normal", table));
    }
    if let Some(targets) = cargo.get("target").and_then(toml::Value::as_table) {
        for (condition, target) in targets {
            if let Some(table) = target.get("dependencies").and_then(toml::Value::as_table) {
                dependency_tables.push((condition.as_str(), table));
            }
        }
    }
    for (condition, table) in dependency_tables {
        for (alias, dependency) in table {
            let dependency = workspace.dependency(alias, dependency)?;
            let policy = dependency.as_table().map_or_else(
                || dependency.clone(),
                |table| {
                    toml::Value::Table(
                        table
                            .iter()
                            .filter(|(key, _)| !matches!(key.as_str(), "path" | "workspace"))
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                    )
                },
            );
            models.insert(
                format!("{name}/Cargo.toml/dependency/{condition}/{alias}"),
                policy.to_string(),
            );
            if let Some(path) = dependency.get("path").and_then(toml::Value::as_str) {
                package(
                    &root.join(path),
                    visited,
                    inputs,
                    models,
                    workspace,
                    dependencies,
                    cache,
                )?;
            } else {
                workspace.external(
                    name,
                    &format!("{condition}/{alias}"),
                    alias,
                    &dependency,
                    dependencies,
                )?;
            }
        }
    }
    Ok(())
}

struct Workspace {
    root: PathBuf,
    manifest: toml::Value,
    lock: toml::Value,
}
fn workspace(root: &Path) -> Result<Workspace, String> {
    for root in root.canonicalize().map_err(|e| e.to_string())?.ancestors() {
        let path = root.join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let manifest: toml::Value = toml::from_str(&read(&path)?).map_err(|e| e.to_string())?;
        if manifest.get("workspace").is_some() {
            let lock =
                toml::from_str(&read(&root.join("Cargo.lock"))?).map_err(|e| e.to_string())?;
            return Ok(Workspace {
                root: root.into(),
                manifest,
                lock,
            });
        }
    }
    Err("wire contract requires a workspace lockfile".into())
}
impl Workspace {
    fn dependency(&self, alias: &str, declaration: &toml::Value) -> Result<toml::Value, String> {
        if declaration.get("workspace").and_then(toml::Value::as_bool) != Some(true) {
            return Ok(declaration.clone());
        }
        let inherited = self
            .manifest
            .get("workspace")
            .and_then(|v| v.get("dependencies"))
            .and_then(|v| v.get(alias))
            .ok_or_else(|| format!("missing workspace dependency {alias}"))?;
        let mut merged = match inherited {
            toml::Value::String(version) => {
                toml::Table::from_iter([("version".into(), version.clone().into())])
            }
            toml::Value::Table(table) => table.clone(),
            _ => return Err(format!("invalid workspace dependency {alias}")),
        };
        for (key, value) in declaration
            .as_table()
            .ok_or("invalid inherited dependency")?
        {
            if key == "workspace" {
                continue;
            }
            if key == "features" {
                let features = merged
                    .get("features")
                    .and_then(toml::Value::as_array)
                    .into_iter()
                    .flatten()
                    .chain(value.as_array().ok_or("invalid dependency features")?)
                    .map(|v| v.as_str().ok_or("invalid feature"))
                    .collect::<Result<BTreeSet<_>, _>>()?
                    .into_iter()
                    .map(|v| toml::Value::String(v.into()))
                    .collect();
                merged.insert(key.clone(), toml::Value::Array(features));
            } else if key == "default-features"
                && inherited.get(key).and_then(toml::Value::as_bool) != Some(false)
            {
                // Cargo cannot disable inherited defaults at the consuming leaf.
                merged.insert(key.clone(), toml::Value::Boolean(true));
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
        if let Some(path) = merged.get("path").and_then(toml::Value::as_str) {
            // Workspace path dependencies are relative to the workspace manifest.
            let absolute = self.root.join(path);
            merged.insert("path".into(), absolute.to_string_lossy().to_string().into());
        }
        Ok(toml::Value::Table(merged))
    }
    fn external(
        &self,
        owner: &str,
        edge_key: &str,
        alias: &str,
        declaration: &toml::Value,
        output: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        let dependency =
            if declaration.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                self.manifest
                    .get("workspace")
                    .and_then(|v| v.get("dependencies"))
                    .and_then(|v| v.get(alias))
                    .ok_or_else(|| format!("missing workspace dependency {alias}"))?
            } else {
                declaration
            };
        let name = dependency
            .get("package")
            .and_then(toml::Value::as_str)
            .unwrap_or(alias);
        let packages = self
            .lock
            .get("package")
            .and_then(toml::Value::as_array)
            .ok_or("missing lock packages")?;
        let locked_owner = packages
            .iter()
            .find(|p| {
                p.get("name").and_then(toml::Value::as_str) == Some(owner)
                    && p.get("source").is_none()
            })
            .ok_or_else(|| format!("missing lock owner {owner}"))?;
        let edges = locked_owner
            .get("dependencies")
            .and_then(toml::Value::as_array);
        let edge = edges
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .find(|edge| edge.split_whitespace().next() == Some(name));
        let Some(edge) = edge else {
            // Disabled optional dependencies have no resolved wire implementation.
            if declaration.get("optional").and_then(toml::Value::as_bool) == Some(true) {
                return Ok(());
            }
            return Err(format!("missing lock edge {owner} -> {name}"));
        };
        let version = edge.split_whitespace().nth(1);
        let resolved: Vec<_> = packages
            .iter()
            .filter(|p| {
                p.get("name").and_then(toml::Value::as_str) == Some(name)
                    && version
                        .is_none_or(|v| p.get("version").and_then(toml::Value::as_str) == Some(v))
            })
            .collect();
        if resolved.len() != 1 {
            return Err(format!("ambiguous lock edge {owner} -> {edge}"));
        }
        let package = resolved[0];
        let mut identity = BTreeMap::new();
        for field in ["name", "version", "source", "checksum"] {
            if let Some(value) = package.get(field).and_then(toml::Value::as_str) {
                identity.insert(field, value);
            }
        }
        output.insert(
            format!("{owner}/{edge_key}"),
            serde_json::to_string(&identity).map_err(|e| e.to_string())?,
        );
        Ok(())
    }
}
fn source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    files.push(directory.to_owned());
    let entries = std::fs::read_dir(directory).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "tests"
            || name == "test_support"
            || name.ends_with("_tests")
            || name.ends_with("_tests.rs")
            || name == "tests.rs"
        {
            continue;
        }
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            source_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}
fn test_only(attributes: &[syn::Attribute]) -> bool {
    fn requires_test(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
                let Ok(parts) = list.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                ) else {
                    return false;
                };
                if list.path.is_ident("all") {
                    parts.iter().any(requires_test)
                } else {
                    !parts.is_empty() && parts.iter().all(requires_test)
                }
            }
            _ => false,
        }
    }
    attributes.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
            || attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|meta| requires_test(&meta))
    })
}
fn serde_model(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|a| {
        let tokens = a.meta.to_token_stream().to_string();
        a.path().is_ident("serde")
            || ((a.path().is_ident("derive") || a.path().is_ident("cfg_attr"))
                && (tokens.contains("Serialize") || tokens.contains("Deserialize")))
    })
}
fn integer_repr(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("repr")
            && attribute
                .meta
                .to_token_stream()
                .to_string()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| {
                    matches!(
                        word,
                        "u8" | "u16"
                            | "u32"
                            | "u64"
                            | "u128"
                            | "i8"
                            | "i16"
                            | "i32"
                            | "i64"
                            | "i128"
                            | "usize"
                            | "isize"
                    )
                })
    })
}
fn collect_codec_types(items: &[syn::Item], names: &mut BTreeSet<String>) {
    for item in items {
        match item {
            syn::Item::Struct(value) if !test_only(&value.attrs) && serde_model(&value.attrs) => {
                names.insert(value.ident.to_string());
            }
            syn::Item::Enum(value)
                if !test_only(&value.attrs)
                    && (serde_model(&value.attrs) || integer_repr(&value.attrs)) =>
            {
                names.insert(value.ident.to_string());
            }
            syn::Item::Impl(value) if !test_only(&value.attrs) => {
                let custom_serde = value.trait_.as_ref().is_some_and(|(_, path, _)| {
                    path.segments.last().is_some_and(|segment| {
                        matches!(
                            segment.ident.to_string().as_str(),
                            "Serialize" | "Deserialize"
                        )
                    })
                });
                let binary_codec = value.items.iter().any(|member|
                    matches!(member, syn::ImplItem::Fn(method) if !test_only(&method.attrs)
                        && wire_function(&method.sig.ident.to_string(), &method.block.to_token_stream().to_string())));
                if (custom_serde || binary_codec)
                    && let syn::Type::Path(path) = value.self_ty.as_ref()
                    && let Some(segment) = path.path.segments.last()
                {
                    names.insert(segment.ident.to_string());
                }
            }
            syn::Item::Mod(value) if !test_only(&value.attrs) => {
                if let Some((_, children)) = &value.content {
                    collect_codec_types(children, names);
                }
            }
            _ => {}
        }
    }
}
fn admission_method(name: &str) -> bool {
    matches!(
        name,
        "new" | "try_from" | "from" | "default" | "parse" | "read" | "write"
    ) || name.starts_with("validate")
        || name.starts_with("is_valid")
}
fn impl_owner(value: &syn::ItemImpl, owners: &BTreeSet<String>) -> bool {
    matches!(value.self_ty.as_ref(),syn::Type::Path(path) if path.path.segments.last().is_some_and(|part|owners.contains(&part.ident.to_string())))
}
fn identifiers(tokens: proc_macro2::TokenStream) -> BTreeSet<String> {
    fn visit(tokens: proc_macro2::TokenStream, result: &mut BTreeSet<String>) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Ident(name) => {
                    result.insert(name.to_string());
                }
                proc_macro2::TokenTree::Group(group) => visit(group.stream(), result),
                _ => {}
            }
        }
    }
    let mut result = BTreeSet::new();
    visit(tokens, &mut result);
    result
}
fn reachable_helpers(
    files: &[(String, Vec<syn::Item>)],
    owners: &BTreeSet<String>,
) -> BTreeSet<String> {
    fn collect(
        items: &[syn::Item],
        owners: &BTreeSet<String>,
        functions: &mut BTreeMap<String, BTreeSet<String>>,
        roots: &mut BTreeSet<String>,
    ) {
        for item in items {
            match item {
                syn::Item::Fn(value) if !test_only(&value.attrs) => {
                    let name = value.sig.ident.to_string();
                    let references = identifiers(value.block.to_token_stream());
                    if wire_function(&name, &value.block.to_token_stream().to_string())
                        || name.starts_with("validate")
                    {
                        roots.insert(name.clone());
                        roots.extend(references.clone());
                    }
                    functions.entry(name).or_default().extend(references);
                }
                syn::Item::Impl(value) if !test_only(&value.attrs) => {
                    for item in &value.items {
                        if let syn::ImplItem::Fn(method) = item
                            && !test_only(&method.attrs)
                            && ((impl_owner(value, owners)
                                && admission_method(&method.sig.ident.to_string()))
                                || wire_function(
                                    &method.sig.ident.to_string(),
                                    &method.block.to_token_stream().to_string(),
                                )
                                || value.trait_.as_ref().is_some_and(|(_, path, _)| {
                                    path.segments.last().is_some_and(|part| {
                                        matches!(
                                            part.ident.to_string().as_str(),
                                            "Serialize" | "Deserialize"
                                        )
                                    })
                                }))
                        {
                            roots.extend(identifiers(method.block.to_token_stream()));
                        }
                    }
                }
                syn::Item::Mod(value) if !test_only(&value.attrs) => {
                    if let Some((_, items)) = &value.content {
                        collect(items, owners, functions, roots);
                    }
                }
                _ => {}
            }
        }
    }
    let mut functions = BTreeMap::<String, BTreeSet<String>>::new();
    let mut roots = BTreeSet::new();
    for (_, items) in files {
        collect(items, owners, &mut functions, &mut roots);
    }
    loop {
        let previous = roots.len();
        for name in roots.clone() {
            if let Some(references) = functions.get(&name) {
                roots.extend(references.iter().cloned());
            }
        }
        if roots.len() == previous {
            break;
        }
    }
    roots.retain(|name| functions.contains_key(name));
    roots
}
#[allow(clippy::too_many_lines)] // One AST classifier keeps model and codec inclusion policy together.
fn items(
    module: &str,
    items: Vec<syn::Item>,
    models: &mut BTreeMap<String, String>,
    codec_types: &BTreeSet<String>,
    helpers: &BTreeSet<String>,
) {
    for mut item in items {
        let key = match &item {
            syn::Item::Struct(value)
                if !test_only(&value.attrs)
                    && (serde_model(&value.attrs)
                        || codec_types.contains(&value.ident.to_string())) =>
            {
                Some(format!("struct/{}", value.ident))
            }
            syn::Item::Enum(value)
                if !test_only(&value.attrs)
                    && (serde_model(&value.attrs)
                        || integer_repr(&value.attrs)
                        || codec_types.contains(&value.ident.to_string())) =>
            {
                Some(format!("enum/{}", value.ident))
            }
            syn::Item::Type(value) if !test_only(&value.attrs) => {
                Some(format!("type/{}", value.ident))
            }
            syn::Item::Const(value) if !test_only(&value.attrs) => {
                Some(format!("constant/{}", value.ident))
            }
            syn::Item::Fn(value)
                if !test_only(&value.attrs)
                    && (helpers.contains(&value.sig.ident.to_string())
                        || wire_function(
                            &value.sig.ident.to_string(),
                            &value.block.to_token_stream().to_string(),
                        )) =>
            {
                Some(format!("codec-function/{}", value.sig.ident))
            }
            syn::Item::Impl(value) if !test_only(&value.attrs) => {
                let codec = value.trait_.as_ref().filter(|(_, path, _)| {
                    path.segments.last().is_some_and(|segment| {
                        matches!(
                            segment.ident.to_string().as_str(),
                            "Serialize" | "Deserialize"
                        )
                    })
                });
                if let Some((_, path, _)) = codec {
                    Some(format!(
                        "codec/{}/{}",
                        value.self_ty.to_token_stream(),
                        path.to_token_stream()
                    ))
                } else {
                    for member in &value.items {
                        if let syn::ImplItem::Fn(method) = member
                            && !test_only(&method.attrs)
                            && ((impl_owner(value, codec_types)
                                && admission_method(&method.sig.ident.to_string()))
                                || wire_function(
                                    &method.sig.ident.to_string(),
                                    &method.block.to_token_stream().to_string(),
                                ))
                        {
                            let mut method = method.clone();
                            StripDocs.visit_impl_item_fn_mut(&mut method);
                            models.insert(
                                format!(
                                    "{module}/codec-method/{}/{}",
                                    value.self_ty.to_token_stream(),
                                    method.sig.ident
                                ),
                                method.to_token_stream().to_string(),
                            );
                        }
                    }
                    None
                }
            }
            syn::Item::Macro(value) if !test_only(&value.attrs) => {
                // Item macros can generate Serde models through a remote definition.
                // Keeping invocations avoids silently omitting their concrete type list.
                Some(format!(
                    "macro/{}",
                    value.ident.as_ref().map_or_else(
                        || hex::encode(Sha256::digest(value.mac.tokens.to_string())),
                        ToString::to_string
                    )
                ))
            }
            syn::Item::Mod(value) if !test_only(&value.attrs) => {
                if let Some((_, children)) = value.content.clone() {
                    self::items(
                        &format!("{module}/{}", value.ident),
                        children,
                        models,
                        codec_types,
                        helpers,
                    );
                }
                None
            }
            _ => None,
        };
        if let Some(key) = key {
            StripDocs.visit_item_mut(&mut item);
            let value = item.to_token_stream().to_string();
            let mut key = format!("{module}/{key}");
            if models.contains_key(&key) {
                key.push('/');
                key.push_str(&hex::encode(Sha256::digest(&value)));
            }
            models.insert(key, value);
        }
    }
}
fn wire_function(name: &str, body: &str) -> bool {
    name.starts_with("encode")
        || name.starts_with("decode")
        || matches!(
            name,
            "read_frame" | "write_frame" | "read_message" | "write_message"
        )
        || body.contains("OperationSpec {")
}
fn item_test_only(item: &syn::Item) -> bool {
    match item {
        syn::Item::Fn(value) => test_only(&value.attrs),
        syn::Item::Mod(value) => test_only(&value.attrs),
        syn::Item::Const(value) => test_only(&value.attrs),
        syn::Item::Struct(value) => test_only(&value.attrs),
        syn::Item::Enum(value) => test_only(&value.attrs),
        syn::Item::Impl(value) => test_only(&value.attrs),
        _ => false,
    }
}
struct StripDocs;
fn strip(attributes: &mut Vec<syn::Attribute>) {
    attributes.retain(|a| !a.path().is_ident("doc"));
}
impl syn::visit_mut::VisitMut for StripDocs {
    fn visit_file_mut(&mut self, value: &mut syn::File) {
        value.items.retain(|item| !item_test_only(item));
        syn::visit_mut::visit_file_mut(self, value);
    }
    fn visit_item_mod_mut(&mut self, value: &mut syn::ItemMod) {
        strip(&mut value.attrs);
        if let Some((_, items)) = &mut value.content {
            items.retain(|item| !item_test_only(item));
        }
        syn::visit_mut::visit_item_mod_mut(self, value);
    }
    fn visit_item_const_mut(&mut self, value: &mut syn::ItemConst) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_const_mut(self, value);
    }
    fn visit_item_fn_mut(&mut self, value: &mut syn::ItemFn) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_fn_mut(self, value);
    }
    fn visit_item_struct_mut(&mut self, value: &mut syn::ItemStruct) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_struct_mut(self, value);
    }
    fn visit_item_enum_mut(&mut self, value: &mut syn::ItemEnum) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_enum_mut(self, value);
    }
    fn visit_item_impl_mut(&mut self, value: &mut syn::ItemImpl) {
        strip(&mut value.attrs);
        value.items.retain(|item| match item {
            syn::ImplItem::Fn(value) => !test_only(&value.attrs),
            syn::ImplItem::Const(value) => !test_only(&value.attrs),
            syn::ImplItem::Type(value) => !test_only(&value.attrs),
            _ => true,
        });
        syn::visit_mut::visit_item_impl_mut(self, value);
    }
    fn visit_item_type_mut(&mut self, value: &mut syn::ItemType) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_type_mut(self, value);
    }
    fn visit_item_macro_mut(&mut self, value: &mut syn::ItemMacro) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_item_macro_mut(self, value);
    }
    fn visit_impl_item_fn_mut(&mut self, value: &mut syn::ImplItemFn) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_impl_item_fn_mut(self, value);
    }
    fn visit_field_mut(&mut self, value: &mut syn::Field) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_field_mut(self, value);
    }
    fn visit_variant_mut(&mut self, value: &mut syn::Variant) {
        strip(&mut value.attrs);
        syn::visit_mut::visit_variant_mut(self, value);
    }
    fn visit_fields_named_mut(&mut self, value: &mut syn::FieldsNamed) {
        if !value.named.is_empty() && !value.named.trailing_punct() {
            value.named.push_punct(syn::token::Comma::default());
        }
        syn::visit_mut::visit_fields_named_mut(self, value);
    }
    fn visit_fields_unnamed_mut(&mut self, value: &mut syn::FieldsUnnamed) {
        if !value.unnamed.is_empty() && !value.unnamed.trailing_punct() {
            value.unnamed.push_punct(syn::token::Comma::default());
        }
        syn::visit_mut::visit_fields_unnamed_mut(self, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contract_identity_matches_shared_corpus() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../protocol/descriptor-conformance.json"))
                .unwrap();
        for mut case in cases {
            let valid = case
                .as_object_mut()
                .unwrap()
                .remove("valid")
                .unwrap()
                .as_bool()
                .unwrap();
            let result = serde_json::from_value::<Contract>(case.clone());
            assert_eq!(
                result.is_ok_and(|value| valid_identity(&value)),
                valid,
                "{case}"
            );
        }
    }
    #[test]
    fn additional_codec_sources_change_digest_and_cannot_escape_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            "version=4\n[[package]]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        let contract = "name='fixture.wire'\nversion=1\nsources=['peer.js']\n";
        std::fs::write(root.join("wire-contract.toml"), contract).unwrap();
        std::fs::write(root.join("peer.js"), "export const limit = 8;\r\n").unwrap();
        let initial = generate(&root).unwrap();
        assert!(
            initial
                .inputs
                .contains(&root.join("src").canonicalize().unwrap()),
            "new source files must invalidate Cargo generation"
        );
        assert!(String::from_utf8_lossy(&initial.bytes).contains("\n  \"models\": {"));
        let mut cached = Generator::default();
        assert_eq!(initial.bytes, cached.generate(&root).unwrap().bytes);
        let parsed = cached.fragments.len();
        assert_eq!(initial.bytes, cached.generate(&root).unwrap().bytes);
        assert_eq!(cached.fragments.len(), parsed);
        assert_eq!(parsed, 1);

        std::fs::write(root.join("peer.js"), "export const limit = 8;\n").unwrap();
        assert_eq!(initial.digest, generate(&root).unwrap().digest);
        std::fs::write(root.join("peer.js"), "export const limit = 9;\n").unwrap();
        assert_ne!(initial.digest, generate(&root).unwrap().digest);
        std::fs::write(directory.path().join("outside.js"), "").unwrap();
        std::fs::write(
            root.join("wire-contract.toml"),
            contract.replace("peer.js", "../outside.js"),
        )
        .unwrap();
        assert!(generate(&root).unwrap_err().contains("escapes workspace"));
        std::fs::write(root.join("wire-contract.toml"), contract).unwrap();
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(outside.join("src")).unwrap();
        std::fs::write(
            outside.join("Cargo.toml"),
            "[package]\nname='outside'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(outside.join("src/lib.rs"), "").unwrap();
        let manifest = root.join("Cargo.toml");
        let source = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            manifest,
            format!("{source}\n[dependencies]\noutside={{path='../outside'}}\n"),
        )
        .unwrap();
        assert!(generate(&root).unwrap_err().contains("escapes workspace"));
    }
    #[test]
    fn conditional_serde_models_are_included_and_test_only_modules_are_excluded() {
        let source = r#"
            #[cfg_attr(feature="wire",derive(Serialize))] struct Input { value:u16 }
            #[cfg(all(test,unix))] mod fixtures { const LIMIT:usize=3; }
        "#;
        assert_ne!(
            snapshot(source),
            snapshot(&source.replace("value:u16", "value:u32"))
        );
        assert_eq!(
            snapshot(source),
            snapshot(&source.replace("LIMIT:usize=3", "LIMIT:usize=4"))
        );
    }
    #[test]
    fn constructors_and_nested_admission_helpers_change_the_contract() {
        let source = r"
            #[derive(Serialize,Deserialize)] struct Input { name:String }
            impl Input { fn new(name:String)->Option<Self> { permitted(&name).then_some(Self{name}) } }
            fn permitted(name:&str)->bool { within_limit(name.len()) }
            fn within_limit(length:usize)->bool { length <= 8 }
            fn paint()->u8 { 3 }
        ";
        assert_ne!(
            snapshot(source),
            snapshot(&source.replace("length <= 8", "length <= 16"))
        );
        assert_eq!(
            snapshot(source),
            snapshot(&source.replace("fn paint()->u8 { 3 }", "fn paint()->u8 { 4 }"))
        );
    }
    #[test]
    fn inherited_dependency_features_are_merged_without_losing_leaf_additions() {
        let workspace=Workspace { root:"/fixture".into(),
            manifest:toml::from_str("[workspace.dependencies]\nserde={version='1',default-features=false,features=['derive']}\n").unwrap(),
            lock:toml::from_str("").unwrap() };
        let leaf: toml::Value =
            toml::from_str("workspace=true\nfeatures=['alloc','derive']\n").unwrap();
        let resolved = workspace.dependency("serde", &leaf).unwrap();
        assert_eq!(
            resolved["features"].as_array().unwrap(),
            &vec![
                toml::Value::String("alloc".into()),
                toml::Value::String("derive".into())
            ]
        );
        assert_eq!(resolved["default-features"].as_bool(), Some(false));
    }
    #[test]
    fn codec_named_test_functions_and_sibling_test_directories_do_not_affect_admission() {
        let source = "#[tokio::test] async fn decoded_jobs_retention() { panic!(\"first\"); }";
        assert!(snapshot(source).is_empty());
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("client_tests")).unwrap();
        std::fs::write(directory.path().join("client_tests/jobs.rs"), source).unwrap();
        std::fs::write(directory.path().join("lib.rs"), "").unwrap();
        let mut files = Vec::new();
        source_files(directory.path(), &mut files).unwrap();
        assert_eq!(
            files,
            vec![directory.path().to_owned(), directory.path().join("lib.rs")]
        );
    }
    #[test]
    fn native_build_refuses_missing_or_stale_reviewed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wire-schema.json");
        assert!(require_reviewed(&path, b"reviewed").is_err());
        std::fs::write(&path, b"reviewed").unwrap();
        require_reviewed(&path, b"reviewed").unwrap();
        assert!(
            require_reviewed(&path, b"changed")
                .unwrap_err()
                .contains("stale wire snapshot")
        );
    }
    fn snapshot(source: &str) -> BTreeMap<String, String> {
        let mut models = BTreeMap::new();
        let parsed = syn::parse_file(source).unwrap();
        let mut codec_types = BTreeSet::new();
        collect_codec_types(&parsed.items, &mut codec_types);
        let helpers = reachable_helpers(&[("fixture".into(), parsed.items.clone())], &codec_types);
        items("fixture", parsed.items, &mut models, &codec_types, &helpers);
        models
    }

    #[test]
    fn schema_tracks_binary_header_types_and_discriminants_without_unrelated_ui_state() {
        let source = r"
            #[derive(Clone, Copy)] #[repr(u8)] enum Kind { Request = 1, Reply = 2 }
            struct Header { kind: Kind, length: u16 }
            impl Header {
                fn encode(&self) -> Vec<u8> {
                    let mut bytes = vec![self.kind as u8];
                    bytes.extend(self.length.to_be_bytes());
                    bytes
                }
            }
            struct View { color: u8 }
        ";
        let before = snapshot(source);
        assert_ne!(
            before,
            snapshot(&source.replace("Request = 1", "Request = 3"))
        );
        assert_ne!(
            before,
            snapshot(&source.replace("length: u16", "length: u32"))
        );
        assert_eq!(
            before,
            snapshot(&source.replace("color: u8", "color: String"))
        );
    }

    #[test]
    fn schema_tracks_serde_attributes_and_custom_codecs_but_not_docs_or_spacing() {
        let first = snapshot("#[derive(Serialize)] pub struct Input { pub name: String }");
        assert_eq!(
            first,
            snapshot(
                "/// docs\n#[derive(Serialize)] pub struct Input {\n/// field\npub name : String,\n}"
            )
        );
        assert_ne!(
            first,
            snapshot(
                "#[derive(Serialize)] pub struct Input { #[serde(rename=\"title\")] pub name: String }"
            )
        );
        assert_ne!(
            first,
            snapshot("#[derive(Serialize)] pub struct Input { pub name: u64 }")
        );
        assert!(!snapshot("impl Serialize for Input { fn serialize() { encode(1); } }").is_empty());
        assert!(
            snapshot("#[cfg(test)] mod tests { #[derive(Serialize)] struct Input; }").is_empty()
        );
    }
}
