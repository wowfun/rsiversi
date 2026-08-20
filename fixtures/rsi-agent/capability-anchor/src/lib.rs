use std::fmt;

use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    Frame, FrameBody, Lane, LifecyclePhase, PostFrameOutcome, RUNTIME_TICK_EVENT,
    RUNTIME_TICK_SERVICE,
};

struct CapabilityAnchor {
    host: Host,
    prepared: Option<u64>,
    committed: Option<u64>,
    pending_retired: Option<u64>,
}

#[derive(Debug)]
struct AnchorError(&'static str);

impl fmt::Display for AnchorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl CapabilityAnchor {
    fn post_outcome(&self, lane: Lane, frame: &Frame) -> Result<PostFrameOutcome, AnchorError> {
        let bytes = frame.encode().map_err(|_| AnchorError("encode frame"))?;
        self.host
            .post_frame(lane, &bytes)
            .map_err(|_| AnchorError("host unavailable"))
    }

    fn post(&self, lane: Lane, frame: &Frame) -> Result<(), AnchorError> {
        match self.post_outcome(lane, frame)? {
            PostFrameOutcome::Accepted => Ok(()),
            PostFrameOutcome::WouldBlock => Err(AnchorError("host backpressure")),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(AnchorError("host closed"))
            }
        }
    }

    fn flush_retired(&mut self) -> Result<(), AnchorError> {
        let Some(generation) = self.pending_retired else {
            return Ok(());
        };
        match self.post_outcome(
            Lane::Control,
            &Frame::lifecycle(LifecyclePhase::Retired, generation, None),
        )? {
            PostFrameOutcome::Accepted => {
                self.pending_retired = None;
                Ok(())
            }
            PostFrameOutcome::WouldBlock => Ok(()),
            PostFrameOutcome::Closed | PostFrameOutcome::Unknown(_) => {
                Err(AnchorError("host closed"))
            }
        }
    }
}

impl Plugin for CapabilityAnchor {
    type Error = AnchorError;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            prepared: None,
            committed: None,
            pending_retired: None,
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let frame = Frame::decode(payload).map_err(|_| AnchorError("invalid frame"))?;
        match frame.body {
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Prepare,
                generation,
                ..
            } if lane == Lane::Control => {
                self.prepared = Some(generation);
                if let Err(error) = self.post(
                    Lane::Control,
                    &Frame::lifecycle(LifecyclePhase::Prepared, generation, None),
                ) {
                    self.prepared = None;
                    return Err(error);
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Abort,
                generation,
                ..
            } if lane == Lane::Control => {
                if self.prepared == Some(generation) {
                    self.prepared = None;
                }
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Committed,
                generation,
                ..
            } if lane == Lane::Control && self.prepared == Some(generation) => {
                self.prepared = None;
                self.committed = Some(generation);
                Ok(())
            }
            FrameBody::Lifecycle {
                phase: LifecyclePhase::Retire,
                generation,
                ..
            } if lane == Lane::Control && self.committed == Some(generation) => {
                self.committed = None;
                self.pending_retired = Some(generation);
                self.flush_retired()
            }
            FrameBody::ServiceEvent {
                service,
                event,
                payload,
                ..
            } if matches!(lane, Lane::Control | Lane::Data)
                && service == RUNTIME_TICK_SERVICE
                && event == RUNTIME_TICK_EVENT
                && payload
                    .get("tick")
                    .and_then(serde_json::Value::as_u64)
                    .is_some() =>
            {
                self.flush_retired()
            }
            _ => Err(AnchorError("frame rejected in current lifecycle state")),
        }
    }
}

rsi_meta_plugin::export_plugin!(CapabilityAnchor);
