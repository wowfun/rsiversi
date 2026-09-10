//! Native cell renderer; the process-wide terminal owner stays resident.
use rsi_meta_native::{
    Activation, Message, NativeInstance, NativePlugin, Prepared, ProviderChannel, export_plugin,
};
use rsi_terminal_ui::{scene::Scene, wire};
#[derive(Default)]
struct Plugin;
impl NativePlugin for Plugin {
    type Prepared = ();
    type Instance = Instance;
    fn identity(&self) -> Result<String, String> {
        Ok("rsi.terminal.native".into())
    }
    fn prepare(&self, desired: &serde_json::Value) -> Result<Prepared<()>, String> {
        if !desired.is_null() {
            return Err("terminal native configuration must be null".into());
        }
        Ok(Prepared::new(desired.clone(), (), 0))
    }
    fn create(&self, (): ()) -> Result<Instance, String> {
        Ok(Instance(rsi_terminal_ui::scene::Renderer::default()))
    }
}
struct Instance(rsi_terminal_ui::scene::Renderer);
impl NativeInstance for Instance {
    fn activate(&mut self, activation: &mut Activation<'_>) -> Result<(), String> {
        activation
            .effects()
            .provide(
                wire::SERVICE,
                wire::CONTRACT,
                u64::from(wire::VERSION),
                b"render",
            )
            .map_err(|e| e.to_string())?;
        activation.effects().commit().map_err(|e| e.to_string())
    }
    fn serve(&mut self, port: &[u8], channel: &mut ProviderChannel<'_>) -> Result<(), String> {
        if port != b"render" {
            return Err("unknown terminal port".into());
        }
        let first = channel
            .receive()
            .map_err(|e| e.to_string())?
            .ok_or("missing terminal model")?;
        if !first.capabilities.is_empty() {
            return Err("unexpected terminal grant".into());
        }
        let request = wire::parse_header(&first.bytes).map_err(str::to_owned)?;
        let mut source = Vec::new();
        while let Some(message) = channel.receive().map_err(|e| e.to_string())? {
            if !message.capabilities.is_empty()
                || message.bytes.is_empty()
                || message.bytes.len() > wire::MAXIMUM_FRAGMENT
                || message.bytes.len() > request.bytes - source.len()
            {
                return Err("invalid terminal source fragment".into());
            }
            source.extend_from_slice(&message.bytes);
        }
        if source.len() != request.bytes {
            return Err("truncated terminal source".into());
        }
        let (mut buffer, view) = Scene::decode(&source)
            .and_then(|scene| self.0.render(scene, request.width, request.height))
            .map_err(str::to_owned)?;
        if cfg!(feature = "revision-b") {
            buffer[(0, 0)].set_symbol("B");
        }
        let frame = wire::encode(request.identity, &buffer, &view).map_err(str::to_owned)?;
        for chunk in frame.chunks(wire::MAXIMUM_FRAGMENT) {
            channel
                .send(&Message {
                    bytes: chunk.to_vec(),
                    capabilities: vec![],
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
export_plugin!(Plugin);
