use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

/// Closed outcome independent of model completion claims.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pass,
    AssertionFailed,
    TargetUnavailable,
    Timeout,
    PolicyBlocked,
    InfrastructureFailed,
    Cancelled,
}

/// Operator-selected predicates; never supplied by a page or model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    TextVisible { text: String },
    RoleVisible { role: String, name: String },
    FinalUrl { url: String },
}

/// Frozen input for one checker scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub entry_identity: String,
    pub assertions: Vec<Assertion>,
}
impl CheckSpec {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the complete policy checks at their owning boundary"
    )]
    pub fn validate(&self) -> Result<(), String> {
        bounded(&self.entry_identity, 1024)?;
        if self.assertions.is_empty() || self.assertions.len() > 16 {
            return Err("checks require 1..16 assertions".into());
        }
        for assertion in &self.assertions {
            match assertion {
                Assertion::TextVisible { text } => bounded(text, 1024)?,
                Assertion::RoleVisible { role, name } => {
                    bounded(role, 64)?;
                    if !matches!(
                        role.as_str(),
                        "alert"
                            | "alertdialog"
                            | "application"
                            | "article"
                            | "banner"
                            | "blockquote"
                            | "button"
                            | "caption"
                            | "cell"
                            | "checkbox"
                            | "code"
                            | "columnheader"
                            | "combobox"
                            | "complementary"
                            | "contentinfo"
                            | "definition"
                            | "deletion"
                            | "dialog"
                            | "directory"
                            | "document"
                            | "emphasis"
                            | "feed"
                            | "figure"
                            | "form"
                            | "generic"
                            | "grid"
                            | "gridcell"
                            | "group"
                            | "heading"
                            | "img"
                            | "insertion"
                            | "link"
                            | "list"
                            | "listbox"
                            | "listitem"
                            | "log"
                            | "main"
                            | "marquee"
                            | "math"
                            | "meter"
                            | "menu"
                            | "menubar"
                            | "menuitem"
                            | "menuitemcheckbox"
                            | "menuitemradio"
                            | "navigation"
                            | "none"
                            | "note"
                            | "option"
                            | "paragraph"
                            | "presentation"
                            | "progressbar"
                            | "radio"
                            | "radiogroup"
                            | "region"
                            | "row"
                            | "rowgroup"
                            | "rowheader"
                            | "scrollbar"
                            | "search"
                            | "searchbox"
                            | "separator"
                            | "slider"
                            | "spinbutton"
                            | "status"
                            | "strong"
                            | "subscript"
                            | "superscript"
                            | "switch"
                            | "tab"
                            | "table"
                            | "tablist"
                            | "tabpanel"
                            | "term"
                            | "textbox"
                            | "time"
                            | "timer"
                            | "toolbar"
                            | "tooltip"
                            | "tree"
                            | "treegrid"
                            | "treeitem"
                    ) {
                        return Err("unsupported pinned Playwright role".into());
                    }
                    bounded(name, 1024)?;
                }
                Assertion::FinalUrl { url } => {
                    bounded(url, 4096)?;
                    Url::parse(url).map_err(|_| "invalid assertion URL")?;
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn bounded(value: &str, maximum: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err("invalid bounded browser text".into());
    }
    Ok(())
}

/// Frozen URL space; identity does not confer execution authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPolicy {
    pub entry_url: String,
    pub path_prefix: String,
    pub dependency_hosts: BTreeSet<String>,
}
impl BrowserPolicy {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), String> {
        bounded(&self.entry_url, 4096)?;
        let url = Url::parse(&self.entry_url).map_err(|_| "invalid entry URL")?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || url.port_or_known_default() != Some(443)
        {
            return Err("preview requires anonymous HTTPS on port 443".into());
        }
        if !self.path_prefix.starts_with('/')
            || !self.path_prefix.ends_with('/')
            || self.path_prefix.len() > 1024
            || self.path_prefix.contains('%')
            || self.path_prefix.contains("..")
            || !url.path().starts_with(&self.path_prefix)
        {
            return Err("invalid preview navigation prefix".into());
        }
        validate_host(url.host_str().ok_or("missing preview host")?)?;
        if self.dependency_hosts.len() > 16 {
            return Err("too many dependency hosts".into());
        }
        for host in &self.dependency_hosts {
            validate_host(host)?;
        }
        Ok(())
    }
    /// # Errors
    /// Rejects destinations outside the frozen policy or failed and retired browser exchanges.
    pub fn navigate(&self, value: &str) -> Result<Url, String> {
        self.validate()?;
        let entry = Url::parse(&self.entry_url).map_err(|_| "invalid entry URL")?;
        let target = Url::parse(value).map_err(|_| "invalid navigation URL")?;
        if target.origin() != entry.origin()
            || !target.username().is_empty()
            || target.password().is_some()
            || !target.path().starts_with(&self.path_prefix)
            || target.path().contains('%')
        {
            return Err("navigation outside deployment URL space".into());
        }
        Ok(target)
    }
    pub fn allows_destination(&self, host: &str, port: u16) -> bool {
        port == 443
            && Url::parse(&self.entry_url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .is_some_and(|entry| host == entry || self.dependency_hosts.contains(host))
    }
}
fn validate_host(host: &str) -> Result<(), String> {
    bounded(host, 253)?;
    if !matches!(url::Host::parse(host), Ok(url::Host::Domain(_)))
        || host.ends_with('.')
        || !host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
    {
        return Err("dependency hosts must be exact canonical DNS names".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionResult {
    pub assertion: Assertion,
    pub passed: bool,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub outcome: CheckOutcome,
    pub final_url: String,
    pub assertions: Vec<AssertionResult>,
    pub snapshot: String,
    pub dialogs_dismissed: u32,
    pub evidence_error: Option<String>,
}
impl CheckResult {
    /// # Errors
    /// Rejects malformed values or values exceeding the owning protocol bounds.
    pub fn validate(&self) -> Result<(), String> {
        if self.final_url.len() > 4096
            || self.assertions.len() > 16
            || self.snapshot.len() > 64 * 1024
            || self.evidence_error.as_ref().is_some_and(|s| s.len() > 1024)
            || self.assertions.iter().any(|a| a.detail.len() > 1024)
        {
            return Err("browser result exceeds bounds".into());
        }
        if !self.assertions.is_empty() {
            CheckSpec {
                entry_identity: "Validated result".into(),
                assertions: self
                    .assertions
                    .iter()
                    .map(|a| a.assertion.clone())
                    .collect(),
            }
            .validate()?;
        }
        if self.outcome == CheckOutcome::PolicyBlocked
            && (!self.snapshot.is_empty() || !self.assertions.is_empty())
        {
            return Err("blocked pages cannot supply evidence".into());
        }
        if self.outcome == CheckOutcome::Pass
            && (self.assertions.is_empty() || self.assertions.iter().any(|a| !a.passed))
        {
            return Err("pass requires all assertions".into());
        }
        if self.outcome == CheckOutcome::AssertionFailed
            && !self.assertions.iter().any(|a| !a.passed)
        {
            return Err("assertion failure requires independent failed predicate".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_and_numeric_addresses_are_rejected_at_the_policy_boundary() {
        for host in [
            "169.254.169.254",
            "127.0.0.1",
            "2130706433",
            "0x7f000001",
            "[::1]",
        ] {
            let mut policy = BrowserPolicy {
                entry_url: format!("https://{host}/"),
                path_prefix: "/".into(),
                dependency_hosts: BTreeSet::default(),
            };
            assert!(policy.validate().is_err(), "{host}");
            policy.entry_url = "https://preview.example/".into();
            policy.dependency_hosts.insert(host.into());
            assert!(policy.validate().is_err(), "dependency {host}");
        }
    }
    #[test]
    fn navigation_cannot_escape_its_frozen_deployment() {
        let policy = BrowserPolicy {
            entry_url: "https://preview.example/deploy/7/".into(),
            path_prefix: "/deploy/7/".into(),
            dependency_hosts: BTreeSet::from(["cdn.example".into()]),
        };
        assert!(
            policy
                .navigate("https://preview.example/deploy/7/details")
                .is_ok()
        );
        for url in [
            "https://preview.example/deploy/8/",
            "https://cdn.example/",
            "https://preview.example/deploy/7/%2e%2e/8/",
            "http://preview.example/deploy/7/",
            "https://secret@preview.example/deploy/7/",
        ] {
            assert!(policy.navigate(url).is_err(), "{url}");
        }
        assert!(policy.allows_destination("cdn.example", 443));
        assert!(!policy.allows_destination("127.0.0.1", 443));
        assert!(!policy.allows_destination("cdn.example", 80));
    }
}
