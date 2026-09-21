//! Public SDK assertions for exact-generation ordinary addon lifecycle.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi::{AddonScope, StandardAddonSet};
use rsi_host::{HostBuilder, ProfileProgram};
use rsi_meta::LocalContract;
use std::sync::Arc;

/// Which exact exported service the author's probe is invoking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationProbe {
    /// Initial active Profile generation.
    Initial,
    /// Prior generation retained across a successful replacement.
    Retained,
    /// Newly published Profile generation.
    Replacement,
}

/// Runs activation, retained/current invocation, replacement and clean teardown.
/// The supplied programs must replace the selected Local service generation.
///
/// # Panics
/// Panics on any Host failure, absent export, reused service identity or dirty
/// teardown. Addon semantics are asserted by `probe`, not inferred from metadata.
pub async fn assert_addon_generations<C: LocalContract>(
    addons: &StandardAddonSet,
    role: AddonScope,
    programs: [ProfileProgram; 2],
    probe: impl Fn(GenerationProbe, &C::Service),
) {
    let build = || {
        let mut builder = HostBuilder::without_paths(std::env::consts::OS);
        addons
            .register_into(&mut builder, role)
            .expect("addon role registration");
        builder.build().expect("frozen addon catalog")
    };
    let [initial, replacement] = programs;
    let running = build()
        .start_program(initial)
        .await
        .expect("initial addon activation");
    let retained = running.lookup_local::<C>().expect("initial public export");
    probe(GenerationProbe::Initial, retained.as_ref());
    let update = running
        .updater()
        .submit(
            1,
            build()
                .profile_input(replacement)
                .expect("replacement Profile"),
        )
        .expect("replacement admission");
    update.wait().await.expect("replacement completion");
    let current = running
        .lookup_local::<C>()
        .expect("replacement public export");
    assert!(
        !Arc::ptr_eq(&retained, &current),
        "replacement must publish a new exact service generation"
    );
    probe(GenerationProbe::Retained, retained.as_ref());
    probe(GenerationProbe::Replacement, current.as_ref());
    drop((retained, current));
    assert!(
        running.shutdown().await.is_clean(),
        "addon Host teardown must be clean"
    );
}
