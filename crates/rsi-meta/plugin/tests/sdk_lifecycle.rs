#![allow(unsafe_code)] // Exercises the public native entry/exchange seam.

use core::ffi::c_void;
use rsi_meta_plugin::{
    ABI_MINOR, BasicOutput, BytesInput, BytesOutput, CAP_KIND_FACTORY, CAP_KIND_INSTANCE,
    CAP_KIND_PREPARED, CapId, CapInput, CapOutput, EmptyInput, FrameHeader, HostTable,
    NativeInstance, NativePlugin, PLUGIN_CAP_RELEASE, PLUGIN_CAP_RETAIN, PLUGIN_CREATE,
    PLUGIN_DESTROY_FACTORY, PLUGIN_DESTROY_INSTANCE, PLUGIN_FINALIZE, PLUGIN_IDENTITY,
    PLUGIN_PREPARE, PLUGIN_RELEASE_OUTPUT, PluginTable, PrepareOutput, Prepared, RIGHT_MUTATE,
    RIGHT_RETAIN, RawBytes, ReleaseOutputInput, STATUS_OK, STATUS_PROTOCOL_ERROR,
    ServiceRequirement, TableHeader, plugin_entry,
};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static INSTANCE_DROPS: AtomicUsize = AtomicUsize::new(0);
static FACTORY_DROPS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default)]
struct LifecyclePlugin;

impl Drop for LifecyclePlugin {
    fn drop(&mut self) {
        FACTORY_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

impl NativePlugin for LifecyclePlugin {
    type Prepared = String;
    type Instance = LifecycleInstance;

    fn identity(&self) -> Result<String, String> {
        Ok("fixture.lifecycle".to_owned())
    }

    fn prepare(&self, desired: &Value) -> Result<Prepared<Self::Prepared>, String> {
        let prefix = desired
            .get("prefix")
            .and_then(Value::as_str)
            .ok_or_else(|| "prefix missing".to_owned())?;
        Ok(Prepared::new(
            json!({ "prefix": prefix }),
            prefix.to_owned(),
            u64::try_from(size_of::<String>() + prefix.len()).unwrap(),
        )
        .requiring(ServiceRequirement::new("upstream", "fixture.upstream", 1)))
    }

    fn create(&self, prepared: Self::Prepared) -> Result<Self::Instance, String> {
        Ok(LifecycleInstance(prepared))
    }
}

struct LifecycleInstance(String);

impl Drop for LifecycleInstance {
    fn drop(&mut self) {
        INSTANCE_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

impl NativeInstance for LifecycleInstance {
    fn activate(
        &mut self,
        _activation: &mut rsi_meta_plugin::Activation<'_>,
    ) -> Result<(), String> {
        Err(format!("{} is not activated in this test", self.0))
    }

    fn serve(
        &mut self,
        _port: &[u8],
        _channel: &mut rsi_meta_plugin::ProviderChannel<'_>,
    ) -> Result<(), String> {
        Err("not served in this test".to_owned())
    }
}

unsafe extern "C" fn unused_host_exchange(
    _: *mut c_void,
    _: u32,
    _: *const c_void,
    _: u32,
    _: *mut c_void,
    _: u32,
) -> u32 {
    rsi_meta_plugin::STATUS_UNSUPPORTED
}

fn host() -> HostTable {
    HostTable {
        header: TableHeader::new(ABI_MINOR, HostTable::STRUCT_SIZE),
        issuer: 71,
        state: core::ptr::dangling_mut::<c_void>(),
        exchange: Some(unused_host_exchange),
    }
}

struct OpenPlugin {
    table: PluginTable,
}

impl OpenPlugin {
    fn enter() -> Self {
        Self::enter_with::<LifecyclePlugin>()
    }

    fn enter_with<P: NativePlugin>() -> Self {
        let host = host();
        let mut table = PluginTable::EMPTY;
        // SAFETY: Both tables are live, aligned, and exclusively borrowed for entry.
        let status =
            unsafe { plugin_entry::<P>(&raw const host, &raw mut table, PluginTable::STRUCT_SIZE) };
        assert_eq!(status, STATUS_OK);
        assert!(table.is_compatible_for_host(ABI_MINOR));
        Self { table }
    }

    fn exchange<I, O>(&self, opcode: u32, input: &I, output: &mut O) -> u32 {
        // SAFETY: Entry validated this exchange pointer. The typed frames remain
        // live for the synchronous call and output has its exact capacity.
        unsafe {
            self.table.exchange.expect("entry exchange")(
                self.table.state,
                opcode,
                std::ptr::from_ref(input).cast(),
                u32::try_from(size_of::<I>()).unwrap(),
                std::ptr::from_mut(output).cast(),
                u32::try_from(size_of::<O>()).unwrap(),
            )
        }
    }

    fn status_only<I>(&self, opcode: u32, input: &I) -> u32 {
        // SAFETY: Same exchange contract as `exchange`; this opcode has no output.
        unsafe {
            self.table.exchange.expect("entry exchange")(
                self.table.state,
                opcode,
                std::ptr::from_ref(input).cast(),
                u32::try_from(size_of::<I>()).unwrap(),
                core::ptr::null_mut(),
                0,
            )
        }
    }

    fn release_output(&self, release: rsi_meta_plugin::ReleaseId) -> u32 {
        self.status_only(
            PLUGIN_RELEASE_OUTPUT,
            &ReleaseOutputInput {
                header: FrameHeader::new(u32::try_from(size_of::<ReleaseOutputInput>()).unwrap()),
                release,
            },
        )
    }

    fn retain(&self, capability: rsi_meta_plugin::CapId) -> u32 {
        let mut output = BasicOutput {
            prefix: rsi_meta_plugin::OutputPrefix::empty(
                u32::try_from(size_of::<BasicOutput>()).unwrap(),
            ),
        };
        self.exchange(
            PLUGIN_CAP_RETAIN,
            &CapInput {
                header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
                capability,
            },
            &mut output,
        )
    }
}

struct LifecycleSetup {
    factory_input: CapInput,
    prepared: CapId,
    identity_release: rsi_meta_plugin::ReleaseId,
}

fn prepare_lifecycle(plugin: &OpenPlugin) -> LifecycleSetup {
    assert_eq!(plugin.retain(plugin.table.factory), STATUS_OK);
    let mut identity = BytesOutput {
        prefix: rsi_meta_plugin::OutputPrefix::empty(0),
        bytes: RawBytes::EMPTY,
    };
    let factory_input = cap_input(plugin.table.factory);
    assert_eq!(
        plugin.exchange(PLUGIN_IDENTITY, &factory_input, &mut identity),
        STATUS_OK
    );
    // SAFETY: The output token keeps this validated range live until release.
    let identity_bytes = unsafe {
        std::slice::from_raw_parts(
            identity.bytes.ptr,
            usize::try_from(identity.bytes.len).unwrap(),
        )
    };
    assert_eq!(identity_bytes, b"fixture.lifecycle");

    let desired = br#"{"prefix":"v2:"}"#;
    let mut output: PrepareOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(
            PLUGIN_PREPARE,
            &BytesInput {
                header: FrameHeader::new(u32::try_from(size_of::<BytesInput>()).unwrap()),
                receiver: plugin.table.factory,
                bytes: RawBytes {
                    ptr: desired.as_ptr(),
                    len: u64::try_from(desired.len()).unwrap(),
                },
            },
            &mut output,
        ),
        STATUS_OK
    );
    assert_eq!(output.prepared.kind, CAP_KIND_PREPARED);
    assert_eq!(output.prepared.rights, RIGHT_RETAIN | RIGHT_MUTATE);
    assert_eq!(output.requirement_count, 1);
    assert_eq!(
        output.retained_bytes,
        u64::try_from(size_of::<String>() + 3).unwrap()
    );
    assert_eq!(plugin.retain(output.prepared), STATUS_OK);
    assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);
    assert_eq!(
        plugin.release_output(output.prefix.release),
        STATUS_PROTOCOL_ERROR
    );
    LifecycleSetup {
        factory_input,
        prepared: output.prepared,
        identity_release: identity.prefix.release,
    }
}

fn create_lifecycle_instance(plugin: &OpenPlugin, prepared: CapId) -> CapId {
    let mut instance: CapOutput = unsafe { core::mem::zeroed() };
    let input = cap_input(prepared);
    assert_eq!(
        plugin.exchange(PLUGIN_CREATE, &input, &mut instance),
        STATUS_OK
    );
    assert_eq!(instance.capability.kind, CAP_KIND_INSTANCE);
    assert_eq!(instance.capability.rights, RIGHT_RETAIN | RIGHT_MUTATE);
    assert_eq!(plugin.retain(instance.capability), STATUS_OK);
    assert_eq!(plugin.release_output(instance.prefix.release), STATUS_OK);

    let mut duplicate: CapOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_CREATE, &input, &mut duplicate),
        STATUS_PROTOCOL_ERROR
    );
    assert_ne!(duplicate.prefix.release, rsi_meta_plugin::ReleaseId::EMPTY);
    assert_eq!(plugin.release_output(duplicate.prefix.release), STATUS_OK);
    instance.capability
}

#[test]
fn sdk_lifecycle_is_single_use_and_finalizes_only_after_all_ownership_returns() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    INSTANCE_DROPS.store(0, Ordering::SeqCst);
    FACTORY_DROPS.store(0, Ordering::SeqCst);
    let plugin = OpenPlugin::enter();
    let setup = prepare_lifecycle(&plugin);

    let instance = create_lifecycle_instance(&plugin, setup.prepared);
    let prepared_input = cap_input(setup.prepared);

    let mut basic = BasicOutput {
        prefix: rsi_meta_plugin::OutputPrefix::empty(0),
    };
    assert_eq!(
        plugin.exchange(
            PLUGIN_DESTROY_INSTANCE,
            &CapInput {
                header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
                capability: instance,
            },
            &mut basic,
        ),
        STATUS_OK
    );
    assert_eq!(INSTANCE_DROPS.load(Ordering::SeqCst), 1);

    let mut release_prepared = BasicOutput {
        prefix: rsi_meta_plugin::OutputPrefix::empty(0),
    };
    assert_eq!(
        plugin.exchange(PLUGIN_CAP_RELEASE, &prepared_input, &mut release_prepared,),
        STATUS_OK
    );
    assert_eq!(
        plugin.exchange(PLUGIN_DESTROY_FACTORY, &setup.factory_input, &mut basic),
        STATUS_OK
    );
    assert_eq!(FACTORY_DROPS.load(Ordering::SeqCst), 0);
    assert_eq!(
        plugin.exchange(PLUGIN_DESTROY_FACTORY, &setup.factory_input, &mut basic),
        STATUS_PROTOCOL_ERROR
    );
    assert_ne!(basic.prefix.release, rsi_meta_plugin::ReleaseId::EMPTY);
    assert_eq!(plugin.release_output(basic.prefix.release), STATUS_OK);
    let mut release_factory = BasicOutput {
        prefix: rsi_meta_plugin::OutputPrefix::empty(0),
    };
    assert_eq!(
        plugin.exchange(
            PLUGIN_CAP_RELEASE,
            &setup.factory_input,
            &mut release_factory,
        ),
        STATUS_OK
    );
    assert_eq!(FACTORY_DROPS.load(Ordering::SeqCst), 1);

    let finalize = EmptyInput {
        header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap()),
    };
    assert_eq!(
        plugin.exchange(PLUGIN_FINALIZE, &finalize, &mut basic),
        STATUS_PROTOCOL_ERROR,
        "the retained identity output still blocks finalization"
    );
    assert_eq!(plugin.release_output(basic.prefix.release), STATUS_OK);
    assert_eq!(plugin.release_output(setup.identity_release), STATUS_OK);

    let mut final_output = BasicOutput {
        prefix: rsi_meta_plugin::OutputPrefix::empty(0),
    };
    assert_eq!(
        plugin.exchange(PLUGIN_FINALIZE, &finalize, &mut final_output),
        STATUS_OK
    );
    assert_eq!(
        final_output.prefix.release,
        rsi_meta_plugin::ReleaseId::EMPTY
    );
}

#[repr(C)]
struct ExtendedCapInput {
    known: CapInput,
    future_minor_field: u64,
}

#[test]
fn exchange_accepts_declared_minor_suffix_and_rejects_undeclared_trailing_bytes() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let plugin = OpenPlugin::enter();
    let mut input = ExtendedCapInput {
        known: CapInput {
            header: FrameHeader::new(u32::try_from(size_of::<ExtendedCapInput>()).unwrap()),
            capability: plugin.table.factory,
        },
        future_minor_field: 0xfeed_beef,
    };
    let mut output: BytesOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_IDENTITY, &input, &mut output),
        STATUS_OK
    );
    assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);

    input.known.header.struct_size = u32::try_from(size_of::<CapInput>()).unwrap();
    output = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_IDENTITY, &input, &mut output),
        rsi_meta_plugin::STATUS_INVALID_ARGUMENT
    );
    assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);

    let mut basic: BasicOutput = unsafe { core::mem::zeroed() };
    let factory = CapInput {
        header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
        capability: plugin.table.factory,
    };
    assert_eq!(
        plugin.exchange(PLUGIN_DESTROY_FACTORY, &factory, &mut basic),
        STATUS_OK
    );
    assert_eq!(
        plugin.exchange(
            PLUGIN_FINALIZE,
            &EmptyInput {
                header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap()),
            },
            &mut basic,
        ),
        STATUS_OK
    );
}

#[test]
fn plugin_capabilities_reject_foreign_stale_kind_and_rights_metadata() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let plugin = OpenPlugin::enter();
    let factory = plugin.table.factory;
    for (mut capability, expected) in [
        (
            {
                let mut value = factory;
                value.issuer += 1;
                value
            },
            rsi_meta_plugin::STATUS_WRONG_CAPABILITY,
        ),
        (
            {
                let mut value = factory;
                value.epoch += 1;
                value
            },
            rsi_meta_plugin::STATUS_STALE_CAPABILITY,
        ),
        (
            {
                let mut value = factory;
                value.kind = CAP_KIND_INSTANCE;
                value
            },
            rsi_meta_plugin::STATUS_WRONG_CAPABILITY,
        ),
        (
            {
                let mut value = factory;
                value.rights = RIGHT_MUTATE;
                value
            },
            rsi_meta_plugin::STATUS_WRONG_CAPABILITY,
        ),
    ] {
        let mut output: BytesOutput = unsafe { core::mem::zeroed() };
        let status = plugin.exchange(
            PLUGIN_IDENTITY,
            &CapInput {
                header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
                capability,
            },
            &mut output,
        );
        assert_eq!(status, expected);
        assert_ne!(output.prefix.release, rsi_meta_plugin::ReleaseId::EMPTY);
        assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);
        capability = factory;
        let _ = capability;
    }

    let mut basic: BasicOutput = unsafe { core::mem::zeroed() };
    let input = CapInput {
        header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
        capability: factory,
    };
    assert_eq!(
        plugin.exchange(PLUGIN_DESTROY_FACTORY, &input, &mut basic),
        STATUS_OK
    );
    let finalize = EmptyInput {
        header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap()),
    };
    assert_eq!(
        plugin.exchange(PLUGIN_FINALIZE, &finalize, &mut basic),
        STATUS_OK
    );
}

struct DropBomb(&'static str);

impl Drop for DropBomb {
    fn drop(&mut self) {
        panic!("{} destructor panicked", self.0);
    }
}

struct FactoryDropPlugin {
    _bomb: DropBomb,
}

impl Default for FactoryDropPlugin {
    fn default() -> Self {
        Self {
            _bomb: DropBomb("factory"),
        }
    }
}

impl NativePlugin for FactoryDropPlugin {
    type Prepared = ();
    type Instance = LifecycleInstance;
    fn identity(&self) -> Result<String, String> {
        Ok("fixture.factory-drop".to_owned())
    }
    fn prepare(&self, _: &Value) -> Result<Prepared<()>, String> {
        unreachable!()
    }
    fn create(&self, (): ()) -> Result<Self::Instance, String> {
        unreachable!()
    }
}

#[derive(Default)]
struct PreparedDropPlugin;

impl NativePlugin for PreparedDropPlugin {
    type Prepared = DropBomb;
    type Instance = LifecycleInstance;
    fn identity(&self) -> Result<String, String> {
        Ok("fixture.prepared-drop".to_owned())
    }
    fn prepare(&self, _: &Value) -> Result<Prepared<Self::Prepared>, String> {
        Ok(Prepared::new(
            Value::Null,
            DropBomb("prepared"),
            u64::try_from(size_of::<DropBomb>()).unwrap(),
        ))
    }
    fn create(&self, _: Self::Prepared) -> Result<Self::Instance, String> {
        unreachable!()
    }
}

#[derive(Default)]
struct InstanceDropPlugin;

impl NativePlugin for InstanceDropPlugin {
    type Prepared = ();
    type Instance = InstanceDrop;
    fn identity(&self) -> Result<String, String> {
        Ok("fixture.instance-drop".to_owned())
    }
    fn prepare(&self, _: &Value) -> Result<Prepared<()>, String> {
        Ok(Prepared::new(Value::Null, (), 0))
    }
    fn create(&self, (): ()) -> Result<Self::Instance, String> {
        Ok(InstanceDrop {
            _bomb: DropBomb("instance"),
        })
    }
}

struct InstanceDrop {
    _bomb: DropBomb,
}

impl NativeInstance for InstanceDrop {
    fn activate(&mut self, _: &mut rsi_meta_plugin::Activation<'_>) -> Result<(), String> {
        Ok(())
    }
    fn serve(
        &mut self,
        _: &[u8],
        _: &mut rsi_meta_plugin::ProviderChannel<'_>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn panicking_factory_prepared_and_instance_drops_leave_the_table_finalizable() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let plugin = OpenPlugin::enter_with::<FactoryDropPlugin>();
    let mut basic: BasicOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(
            PLUGIN_DESTROY_FACTORY,
            &cap_input(plugin.table.factory),
            &mut basic,
        ),
        rsi_meta_plugin::STATUS_PANICKED
    );
    assert_eq!(plugin.release_output(basic.prefix.release), STATUS_OK);
    finalize_consumed(&plugin, &mut basic);

    let plugin = OpenPlugin::enter_with::<PreparedDropPlugin>();
    let prepared = prepare_empty(&plugin);
    release_last(
        &plugin,
        prepared,
        rsi_meta_plugin::STATUS_PANICKED,
        &mut basic,
    );
    assert_eq!(
        plugin.exchange(
            PLUGIN_DESTROY_FACTORY,
            &cap_input(plugin.table.factory),
            &mut basic,
        ),
        STATUS_OK
    );
    finalize_consumed(&plugin, &mut basic);

    let plugin = OpenPlugin::enter_with::<InstanceDropPlugin>();
    let prepared = prepare_empty(&plugin);
    let mut instance: CapOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_CREATE, &cap_input(prepared), &mut instance),
        STATUS_OK
    );
    assert_eq!(plugin.retain(instance.capability), STATUS_OK);
    assert_eq!(plugin.release_output(instance.prefix.release), STATUS_OK);
    assert_eq!(
        plugin.exchange(
            PLUGIN_DESTROY_INSTANCE,
            &cap_input(instance.capability),
            &mut basic,
        ),
        rsi_meta_plugin::STATUS_PANICKED
    );
    assert_eq!(plugin.release_output(basic.prefix.release), STATUS_OK);
    release_last(&plugin, prepared, STATUS_OK, &mut basic);
    assert_eq!(
        plugin.exchange(
            PLUGIN_DESTROY_FACTORY,
            &cap_input(plugin.table.factory),
            &mut basic,
        ),
        STATUS_OK
    );
    finalize_consumed(&plugin, &mut basic);
}

fn prepare_empty(plugin: &OpenPlugin) -> CapId {
    let desired = b"{}";
    let mut output: PrepareOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(
            PLUGIN_PREPARE,
            &BytesInput {
                header: FrameHeader::new(u32::try_from(size_of::<BytesInput>()).unwrap()),
                receiver: plugin.table.factory,
                bytes: RawBytes {
                    ptr: desired.as_ptr(),
                    len: u64::try_from(desired.len()).unwrap(),
                },
            },
            &mut output,
        ),
        STATUS_OK
    );
    assert_eq!(plugin.retain(output.prepared), STATUS_OK);
    assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);
    output.prepared
}

fn cap_input(capability: CapId) -> CapInput {
    CapInput {
        header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
        capability,
    }
}

fn release_last(plugin: &OpenPlugin, capability: CapId, expected: u32, output: &mut BasicOutput) {
    assert_eq!(
        plugin.exchange(PLUGIN_CAP_RELEASE, &cap_input(capability), output),
        expected
    );
    if !output.prefix.release.is_empty() {
        assert_eq!(plugin.release_output(output.prefix.release), STATUS_OK);
    }
}

fn finalize_consumed(plugin: &OpenPlugin, output: &mut BasicOutput) {
    assert_eq!(
        plugin.exchange(
            PLUGIN_FINALIZE,
            &EmptyInput {
                header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap()),
            },
            output,
        ),
        STATUS_OK
    );
}

struct PanickingPlugin;

impl Default for PanickingPlugin {
    fn default() -> Self {
        panic!("entry construction panic")
    }
}

impl NativePlugin for PanickingPlugin {
    type Prepared = ();
    type Instance = LifecycleInstance;

    fn identity(&self) -> Result<String, String> {
        unreachable!()
    }

    fn prepare(&self, _: &Value) -> Result<Prepared<Self::Prepared>, String> {
        unreachable!()
    }

    fn create(&self, (): Self::Prepared) -> Result<Self::Instance, String> {
        unreachable!()
    }
}

#[test]
fn malformed_or_panicking_entry_transfers_no_cleanup_authority() {
    let _serial = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut invalid_host = host();
    invalid_host.exchange = None;
    let mut output = plugin(ABI_MINOR);
    // SAFETY: Full table storage is provided; the host table is structurally invalid by design.
    assert_eq!(
        unsafe {
            plugin_entry::<LifecyclePlugin>(
                &raw const invalid_host,
                &raw mut output,
                PluginTable::STRUCT_SIZE,
            )
        },
        rsi_meta_plugin::STATUS_INVALID_ARGUMENT
    );
    assert!(output.state.is_null());
    assert_eq!(output.factory, rsi_meta_plugin::CapId::INVALID);

    output = plugin(ABI_MINOR);
    let valid_host = host();
    // SAFETY: Full table storage is provided; the constructor panic is contained by entry.
    assert_eq!(
        unsafe {
            plugin_entry::<PanickingPlugin>(
                &raw const valid_host,
                &raw mut output,
                PluginTable::STRUCT_SIZE,
            )
        },
        rsi_meta_plugin::STATUS_PANICKED
    );
    assert!(output.state.is_null());
    assert_eq!(output.factory, rsi_meta_plugin::CapId::INVALID);
}

struct PayloadDropPanic;

impl Drop for PayloadDropPanic {
    fn drop(&mut self) {
        panic!("panic payload destructor panicked");
    }
}

struct PayloadEntryPlugin;

impl Default for PayloadEntryPlugin {
    fn default() -> Self {
        std::panic::panic_any(PayloadDropPanic)
    }
}

impl NativePlugin for PayloadEntryPlugin {
    type Prepared = ();
    type Instance = LifecycleInstance;
    fn identity(&self) -> Result<String, String> {
        unreachable!()
    }
    fn prepare(&self, _: &Value) -> Result<Prepared<()>, String> {
        unreachable!()
    }
    fn create(&self, (): ()) -> Result<Self::Instance, String> {
        unreachable!()
    }
}

#[derive(Default)]
struct PayloadOperationPlugin;

impl NativePlugin for PayloadOperationPlugin {
    type Prepared = ();
    type Instance = LifecycleInstance;
    fn identity(&self) -> Result<String, String> {
        std::panic::panic_any(PayloadDropPanic)
    }
    fn prepare(&self, _: &Value) -> Result<Prepared<()>, String> {
        unreachable!()
    }
    fn create(&self, (): ()) -> Result<Self::Instance, String> {
        unreachable!()
    }
}

#[test]
fn panic_payload_destructor_is_contained_at_entry_and_exchange() {
    const SCENARIO: &str = "RSI_META_PLUGIN_PAYLOAD_DROP_SCENARIO";
    if let Ok(scenario) = std::env::var(SCENARIO) {
        if scenario == "entry" {
            let mut table = plugin(ABI_MINOR);
            let host = host();
            // SAFETY: Complete aligned tables exercise the hostile constructor payload.
            assert_eq!(
                unsafe {
                    plugin_entry::<PayloadEntryPlugin>(
                        &raw const host,
                        &raw mut table,
                        PluginTable::STRUCT_SIZE,
                    )
                },
                rsi_meta_plugin::STATUS_PANICKED
            );
            assert!(table.state.is_null());
            return;
        }
        assert_eq!(scenario, "exchange");
        run_payload_operation();
        return;
    }

    for scenario in ["entry", "exchange"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "panic_payload_destructor_is_contained_at_entry_and_exchange",
                "--nocapture",
            ])
            .env(SCENARIO, scenario)
            .status()
            .expect("spawn hostile payload subprocess");
        assert!(
            status.success(),
            "payload scenario {scenario} escaped containment"
        );
    }
}

fn run_payload_operation() {
    let host = host();
    let mut table = PluginTable::EMPTY;
    // SAFETY: Complete aligned tables remain live through finalization.
    assert_eq!(
        unsafe {
            plugin_entry::<PayloadOperationPlugin>(
                &raw const host,
                &raw mut table,
                PluginTable::STRUCT_SIZE,
            )
        },
        STATUS_OK
    );
    let plugin = OpenPlugin { table };
    let input = CapInput {
        header: FrameHeader::new(u32::try_from(size_of::<CapInput>()).unwrap()),
        capability: table.factory,
    };
    let mut bytes: BytesOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_IDENTITY, &input, &mut bytes),
        rsi_meta_plugin::STATUS_PANICKED
    );
    assert_eq!(plugin.release_output(bytes.prefix.release), STATUS_OK);
    let mut basic: BasicOutput = unsafe { core::mem::zeroed() };
    assert_eq!(
        plugin.exchange(PLUGIN_DESTROY_FACTORY, &input, &mut basic),
        STATUS_OK
    );
    assert_eq!(
        plugin.exchange(
            PLUGIN_FINALIZE,
            &EmptyInput {
                header: FrameHeader::new(u32::try_from(size_of::<EmptyInput>()).unwrap()),
            },
            &mut basic,
        ),
        STATUS_OK
    );
}

fn plugin(minor: u32) -> PluginTable {
    PluginTable {
        header: TableHeader::new(minor, PluginTable::STRUCT_SIZE),
        issuer: 99,
        state: core::ptr::dangling_mut(),
        exchange: Some(unused_host_exchange),
        factory: CapId {
            issuer: 99,
            slot: 1,
            epoch: 1,
            kind: CAP_KIND_FACTORY,
            rights: RIGHT_RETAIN | RIGHT_MUTATE,
        },
    }
}
