use super::*;
use rsi_agent_session_protocol::ExecutionCoordinates;
use std::any::Any;

/// Product-issued, non-serializable source/target admission retained through capture.
/// The product owns grants; References independently verifies the bound originals.
pub struct CaptureContext {
    source: ReferenceSource,
    coordinates: ExecutionCoordinates,
    target: SessionHeader,
    _retained: Box<dyn Any + Send + Sync>,
    current: Box<dyn Fn() -> bool + Send + Sync>,
}
impl std::fmt::Debug for CaptureContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureContext")
            .field("source", &self.source)
            .field("target", self.target.session_id())
            .finish_non_exhaustive()
    }
}
impl CaptureContext {
    /// Called by a trusted product after independently admitting both sides.
    /// `retained` owns both admissions and target lifetime; `current` rechecks revocation.
    pub fn new<T: Send + Sync + 'static>(
        source: ReferenceSource,
        coordinates: ExecutionCoordinates,
        target: SessionHeader,
        retained: T,
        current: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        source.validate().map_err(invalid)?;
        target.validate().map_err(invalid)?;
        if target.protection().is_some() {
            return Err(invalid("protected reference target"));
        }
        let context = Self {
            source,
            coordinates,
            target,
            _retained: Box::new(retained),
            current: Box::new(current),
        };
        context.check()?;
        Ok(context)
    }
    /// Exact source bound by admission, independent of caller-supplied hits.
    pub fn source(&self) -> &ReferenceSource {
        &self.source
    }
    /// Exact source execution coordinates, including machine identity.
    pub fn coordinates(&self) -> &ExecutionCoordinates {
        &self.coordinates
    }
    pub(super) fn target(&self) -> &SessionHeader {
        &self.target
    }
    pub(super) fn check(&self) -> Result<()> {
        if (self.current)() {
            Ok(())
        } else {
            Err(ReferenceError::Cancelled)
        }
    }
}
