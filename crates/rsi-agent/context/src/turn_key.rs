//! Qualified internal projection coordinates; JSON map keys use one bounded tuple.
use rsi_agent_session_protocol::{SessionId, TurnId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct TurnKey {
    pub session: SessionId,
    pub turn: TurnId,
}
impl TurnKey {
    pub fn new(session: &SessionId, turn: &TurnId) -> Self {
        Self {
            session: session.clone(),
            turn: turn.clone(),
        }
    }
}
impl Serialize for TurnKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let encoded = serde_json::to_string(&(&self.session, &self.turn))
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&encoded)
    }
}
impl<'de> Deserialize<'de> for TurnKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > 4096 {
            return Err(serde::de::Error::custom(
                "projection coordinate exceeds its bound",
            ));
        }
        let (session, turn) = serde_json::from_str(&encoded).map_err(serde::de::Error::custom)?;
        Ok(Self { session, turn })
    }
}
