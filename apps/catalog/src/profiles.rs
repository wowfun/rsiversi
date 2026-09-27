use super::ApplicationProfileId;
const CLI_PROFILE: &str = "cli";
const HEADLESS_PROFILE: &str = "headless";
const TUI_PROFILE: &str = "tui";
const SERVE_PROFILE: &str = "serve";
const WEB_PROFILE: &str = "web";
const ACP_PROFILE: &str = "acp";
const DEVICES_PROFILE: &str = "devices";
const INSPECTOR_PROFILE: &str = "inspector";
const ADDONS_PROFILE: &str = "addons";
pub(super) fn builtins() -> impl Iterator<Item = (ApplicationProfileId, Vec<u8>)> {
    [
        CLI_PROFILE,
        HEADLESS_PROFILE,
        TUI_PROFILE,
        SERVE_PROFILE,
        WEB_PROFILE,
        ACP_PROFILE,
        DEVICES_PROFILE,
        INSPECTOR_PROFILE,
        ADDONS_PROFILE,
    ]
    .into_iter()
    .map(|name| {
        let id = ApplicationProfileId::new(name).expect("linked Profile ID");
        let bytes = builtin_application(&id).expect("linked Profile");
        (id, bytes)
    })
}
fn builtin_application(id: &ApplicationProfileId) -> Option<Vec<u8>> {
    if id.as_str() == WEB_PROFILE {
        return Some(
            br#"format = 1
[[steps]]
kind = "plugin"
id = "assets"
plugin = "rsi.application.web-assets"
[[steps]]
kind = "plugin"
id = "service"
plugin = "rsi.application.service"
config = { host_profile = "standard" }
[[steps]]
kind = "plugin"
id = "application"
plugin = "rsi.application.web"
"#
            .to_vec(),
        );
    }
    let (plugin, connection) = match id.as_str() {
        CLI_PROFILE => ("rsi.application.cli", "rsi.application.connection"),
        HEADLESS_PROFILE => ("rsi.application.headless", "rsi.application.connection"),
        TUI_PROFILE => ("rsi.application.tui", "rsi.application.connection"),
        SERVE_PROFILE => ("rsi.application.serve", "rsi.application.service"),
        ACP_PROFILE => ("rsi.application.acp", "rsi.application.acp-service"),
        DEVICES_PROFILE => ("rsi.application.devices", "rsi.application.operator"),
        ADDONS_PROFILE => ("rsi.application.addons", "rsi.application.operator"),
        INSPECTOR_PROFILE => ("rsi.application.inspector", "rsi.application.operator"),
        _ => return None,
    };
    let config = if matches!(
        id.as_str(),
        DEVICES_PROFILE | INSPECTOR_PROFILE | ADDONS_PROFILE
    ) {
        ""
    } else {
        "config = { host_profile = \"standard\" }"
    };
    let ui = if id.as_str() == TUI_PROFILE {
        r#"[[steps]]
kind = "plugin"
id = "ui"
plugin = "rsi.ui"
[[steps]]
kind = "plugin"
id = "ui-target"
plugin = "rsi.ui.target"
config = "application"
[[steps]]
kind = "plugin"
id = "setup"
plugin = "rsi.workbench.setup"
[[steps]]
kind = "plugin"
id = "plugins"
plugin = "rsi.workbench.plugins"
[[steps]]
kind = "plugin"
id = "session-ui"
plugin = "rsi.session.ui"
[[steps]]
kind = "plugin"
id = "tree-ui"
plugin = "rsi.session.tree.ui"
[[steps]]
kind = "plugin"
id = "files-ui"
plugin = "rsi.session.files.ui"
[[steps]]
kind = "plugin"
id = "service-ui"
plugin = "rsi.service.ui.client"
[[steps]]
kind = "plugin"
id = "workspace-review-ui"
plugin = "rsi.workspace.review.ui"
"#
    } else {
        ""
    };
    Some(
        format!(
            r#"format = 1
[[steps]]
kind = "plugin"
id = "connection"
plugin = "{connection}"
{config}
{ui}[[steps]]
kind = "plugin"
id = "application"
plugin = "{plugin}"
"#
        )
        .into_bytes(),
    )
}
