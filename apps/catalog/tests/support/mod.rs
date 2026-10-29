use std::{ffi::OsString, sync::Arc};

pub fn embedded_serve(
    service: rsi::StandardComposition,
    arguments: Vec<OsString>,
) -> (rsi_host::Host, rsi_host::ProfileProgram) {
    let mut builder = rsi::StandardAddonBuilder::new("fixture.embedded-serve");
    builder
        .register_factory(
            rsi::AddonScope::Application,
            "fixture.embedded-serve",
            "1",
            rsi_meta::UpdateMode::RestartRequired,
            Arc::new(rsi_serve::ServeFactory::new(arguments)),
        )
        .unwrap();
    let composition = rsi::ApplicationComposition::new(
        service,
        Arc::new(rsi_app_catalog::Catalog::new(
            rsi_serve::WebLaunchEnvironment::default(),
        )),
        rsi::StandardAddonSet::new([builder.build().unwrap()]).unwrap(),
    )
    .unwrap();
    let (host, _) = rsi::standard_application_host(composition, vec![]).unwrap();
    let program = rsi_host::ProfileProgram::from_profile(rsi_host::Profile::new(vec![
        rsi_host::ProfileEntry::new(
            "service",
            "rsi.application.service",
            serde_json::json!({"host_profile":"standard"}),
        ),
        rsi_host::ProfileEntry::new("http", "fixture.embedded-serve", serde_json::Value::Null),
    ]));
    (host, program)
}
