use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use proc_macro2::{TokenStream, TokenTree};
use serde::{Deserialize, Serialize};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Meta, Token};

const BASELINE_PATH: &str = "crates/rsi-meta/code-health.toml";
const HARD_LIMIT: usize = 1_200;
const REGIONS: [&str; 6] = ["core", "runtime", "service", "events", "loader", "abi"];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    version: u8,
    hard_limit: usize,
    regions: BTreeMap<String, usize>,
}

#[derive(Debug)]
struct Measurement {
    path: PathBuf,
    region: &'static str,
    lines: usize,
}

pub fn run(repository: &Path, write: bool) -> Result<(), String> {
    require_repository_root(repository)?;
    let measurements = measure_repository(repository)?;
    let mut maxima = REGIONS
        .into_iter()
        .map(|region| (region.to_owned(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut errors = Vec::new();
    for measurement in &measurements {
        maxima
            .entry(measurement.region.to_owned())
            .and_modify(|maximum| *maximum = (*maximum).max(measurement.lines));
        if measurement.lines > HARD_LIMIT {
            errors.push(format!(
                "{}: {} effective production lines exceed hard limit {HARD_LIMIT}",
                measurement.path.display(),
                measurement.lines
            ));
        }
    }
    if !errors.is_empty() {
        errors.sort();
        return Err(errors.join("\n"));
    }

    let baseline_path = repository.join(BASELINE_PATH);
    if write {
        let regions = if baseline_path.is_file() {
            let previous = read_baseline(&baseline_path)?;
            validate_baseline_shape(&previous)?;
            for region in REGIONS {
                let current = maxima[region];
                let recorded = previous.regions[region];
                if current > recorded {
                    return Err(format!(
                        "region {region} grew from baseline {recorded} to {current}; --write only lowers baselines"
                    ));
                }
            }
            REGIONS
                .into_iter()
                .map(|region| (region.to_owned(), maxima[region]))
                .collect()
        } else {
            maxima.clone()
        };
        let baseline = Baseline {
            version: 1,
            hard_limit: HARD_LIMIT,
            regions,
        };
        let encoded = toml::to_string_pretty(&baseline)
            .map_err(|error| format!("encode {}: {error}", baseline_path.display()))?;
        fs::write(&baseline_path, encoded)
            .map_err(|error| format!("write {}: {error}", baseline_path.display()))?;
    } else {
        let baseline = read_baseline(&baseline_path)?;
        validate_baseline_shape(&baseline)?;
        for region in REGIONS {
            let current = maxima[region];
            let recorded = baseline.regions[region];
            if current > recorded {
                errors.push(format!(
                    "region {region} maximum grew from {recorded} to {current} effective production lines"
                ));
            }
        }
        if !errors.is_empty() {
            errors.sort();
            return Err(errors.join("\n"));
        }
    }

    for region in REGIONS {
        println!("rsi-meta code-health {region}: {}", maxima[region]);
    }
    Ok(())
}

fn require_repository_root(repository: &Path) -> Result<(), String> {
    if repository.join("Cargo.toml").is_file()
        && repository.join("crates/rsi-meta/core/src").is_dir()
        && repository.join("crates/rsi-meta/loader/src").is_dir()
    {
        Ok(())
    } else {
        Err("rsi-meta code-health must run from the repository root".to_owned())
    }
}

fn read_baseline(path: &Path) -> Result<Baseline, String> {
    let source = fs::read_to_string(path).map_err(|error| {
        format!(
            "read {}: {error}; run with --write to create it",
            path.display()
        )
    })?;
    toml::from_str(&source).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn validate_baseline_shape(baseline: &Baseline) -> Result<(), String> {
    if baseline.version != 1 {
        return Err(format!(
            "unsupported code-health baseline version {}",
            baseline.version
        ));
    }
    if baseline.hard_limit != HARD_LIMIT {
        return Err(format!(
            "code-health hard limit must remain {HARD_LIMIT}, found {}",
            baseline.hard_limit
        ));
    }
    let expected = REGIONS.into_iter().collect::<BTreeSet<_>>();
    let actual = baseline
        .regions
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(
            "code-health baseline must contain exactly the six foundation regions".to_owned(),
        );
    }
    Ok(())
}

fn measure_repository(repository: &Path) -> Result<Vec<Measurement>, String> {
    let mut paths = Vec::new();
    collect_rust_files(&repository.join("crates/rsi-meta/core/src"), &mut paths)?;
    collect_rust_files(&repository.join("crates/rsi-meta/loader/src"), &mut paths)?;
    collect_rust_files(&repository.join("crates/rsi-meta/plugin/src"), &mut paths)?;
    paths.sort();
    let excluded_modules = test_only_module_files(&paths)?;
    paths
        .into_iter()
        .filter(|path| !excluded_modules.contains(path))
        .map(|path| {
            let relative = path
                .strip_prefix(repository)
                .map_err(|error| format!("relativize {}: {error}", path.display()))?
                .to_owned();
            Ok(Measurement {
                region: classify(&relative),
                lines: effective_lines(&path)?,
                path: relative,
            })
        })
        .collect()
}

fn test_only_module_files(paths: &[PathBuf]) -> Result<BTreeSet<PathBuf>, String> {
    let mut excluded = BTreeSet::new();
    for path in paths {
        let source = fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let syntax = syn::parse_file(&source)
            .map_err(|error| format!("parse Rust source {}: {error}", path.display()))?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        for item in syntax.items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if module.content.is_some() || !cfg_excludes_production(&module.attrs) {
                continue;
            }
            let name = module.ident.to_string();
            for candidate in [
                parent.join(format!("{name}.rs")),
                parent.join(&name).join("mod.rs"),
            ] {
                if candidate.is_file() {
                    excluded.insert(candidate);
                }
            }
        }
    }
    Ok(excluded)
}

fn collect_rust_files(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("read {} entry: {error}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_files(&path, paths)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn classify(path: &Path) -> &'static str {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with("crates/rsi-meta/loader/src/") {
        "loader"
    } else if normalized.starts_with("crates/rsi-meta/plugin/src/") {
        "abi"
    } else if normalized.contains("/runtime/") || normalized.ends_with("/runtime.rs") {
        "runtime"
    } else if normalized.contains("/service/") || normalized.ends_with("/service.rs") {
        "service"
    } else if normalized.ends_with("/events.rs") {
        "events"
    } else {
        "core"
    }
}

fn effective_lines(path: &Path) -> Result<usize, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let syntax = syn::parse_file(&source)
        .map_err(|error| format!("parse Rust source {}: {error}", path.display()))?;
    let mut excluded = CfgTestRanges::default();
    excluded.visit_file(&syntax);
    let tokens = TokenStream::from_str(&source)
        .map_err(|error| format!("tokenize Rust source {}: {error}", path.display()))?;
    let mut occupied = BTreeSet::new();
    collect_token_lines(tokens, &mut occupied);
    let lines = source.lines().collect::<Vec<_>>();
    Ok(occupied
        .into_iter()
        .filter(|line| !excluded.contains(*line))
        .filter(|line| {
            let trimmed = lines
                .get(line.saturating_sub(1))
                .map_or("", |line| line.trim());
            !trimmed.is_empty()
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("/*")
                && !trimmed.starts_with('*')
                && trimmed != "*/"
        })
        .count())
}

fn collect_token_lines(tokens: TokenStream, occupied: &mut BTreeSet<usize>) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => {
                mark_span(group.span_open(), occupied);
                collect_token_lines(group.stream(), occupied);
                mark_span(group.span_close(), occupied);
            }
            TokenTree::Ident(token) => mark_span(token.span(), occupied),
            TokenTree::Punct(token) => mark_span(token.span(), occupied),
            TokenTree::Literal(token) => mark_span(token.span(), occupied),
        }
    }
}

fn mark_span(span: proc_macro2::Span, occupied: &mut BTreeSet<usize>) {
    let start = span.start().line;
    let end = span.end().line.max(start);
    occupied.extend(start..=end);
}

#[derive(Default)]
struct CfgTestRanges {
    ranges: Vec<(usize, usize)>,
}

impl CfgTestRanges {
    fn contains(&self, line: usize) -> bool {
        self.ranges
            .iter()
            .any(|(start, end)| *start <= line && line <= *end)
    }

    fn exclude(&mut self, attrs: &[syn::Attribute], span: proc_macro2::Span) -> bool {
        let Some(attribute) = attrs
            .iter()
            .find(|attribute| cfg_excludes_production(std::slice::from_ref(*attribute)))
        else {
            return false;
        };
        self.ranges
            .push((attribute.span().start().line, span.end().line));
        true
    }
}

fn cfg_excludes_production(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attribute| {
        let Meta::List(list) = &attribute.meta else {
            return false;
        };
        if !list.path.is_ident("cfg") {
            return false;
        }
        syn::parse2::<Meta>(list.tokens.clone())
            .is_ok_and(|meta| !cfg_possibility(&meta).can_be_true)
    })
}

#[derive(Clone, Copy)]
struct CfgPossibility {
    can_be_true: bool,
    can_be_false: bool,
}

fn cfg_possibility(meta: &Meta) -> CfgPossibility {
    match meta {
        Meta::Path(path) if path.is_ident("test") => CfgPossibility {
            can_be_true: false,
            can_be_false: true,
        },
        Meta::Path(_) | Meta::NameValue(_) => CfgPossibility {
            can_be_true: true,
            can_be_false: true,
        },
        Meta::List(list) => {
            let nested = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .unwrap_or_default()
                .iter()
                .map(cfg_possibility)
                .collect::<Vec<_>>();
            if list.path.is_ident("all") {
                CfgPossibility {
                    can_be_true: nested.iter().all(|value| value.can_be_true),
                    can_be_false: nested.iter().any(|value| value.can_be_false),
                }
            } else if list.path.is_ident("any") {
                CfgPossibility {
                    can_be_true: nested.iter().any(|value| value.can_be_true),
                    can_be_false: nested.iter().all(|value| value.can_be_false),
                }
            } else if list.path.is_ident("not") && nested.len() == 1 {
                CfgPossibility {
                    can_be_true: nested[0].can_be_false,
                    can_be_false: nested[0].can_be_true,
                }
            } else {
                CfgPossibility {
                    can_be_true: true,
                    can_be_false: true,
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for CfgTestRanges {
    fn visit_item(&mut self, node: &'ast syn::Item) {
        let excluded = match node {
            syn::Item::Const(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Enum(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::ExternCrate(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Fn(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::ForeignMod(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Impl(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Macro(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Mod(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Static(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Struct(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Trait(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::TraitAlias(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Type(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Union(item) => self.exclude(&item.attrs, item.span()),
            syn::Item::Use(item) => self.exclude(&item.attrs, item.span()),
            _ => false,
        };
        if !excluded {
            syn::visit::visit_item(self, node);
        }
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        let excluded = match node {
            syn::ImplItem::Const(item) => self.exclude(&item.attrs, item.span()),
            syn::ImplItem::Fn(item) => self.exclude(&item.attrs, item.span()),
            syn::ImplItem::Macro(item) => self.exclude(&item.attrs, item.span()),
            syn::ImplItem::Type(item) => self.exclude(&item.attrs, item.span()),
            _ => false,
        };
        if !excluded {
            syn::visit::visit_impl_item(self, node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_only_guaranteed_test_items_comments_and_blank_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.rs");
        fs::write(
            &path,
            "// comment\n\npub fn kept() {\n    let value = 1; // inline\n}\n\n#[cfg(test)]\nmod tests {\n    fn excluded() {}\n}\n#[cfg(feature = \"test-failpoints\")]\nfn failpoint() {}\n#[cfg(not(feature = \"test-failpoints\"))]\nfn production() {}\n",
        )
        .unwrap();
        assert_eq!(effective_lines(&path).unwrap(), 7);
    }

    #[test]
    fn excludes_files_owned_only_by_a_cfg_test_module() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("lib.rs");
        let gated = directory.path().join("test_support.rs");
        fs::write(&root, "#[cfg(test)]\nmod test_support;\n").unwrap();
        fs::write(&gated, "pub fn gate() {}\n").unwrap();

        let excluded = test_only_module_files(&[root, gated.clone()]).unwrap();
        assert_eq!(excluded, BTreeSet::from([gated]));
    }
}
