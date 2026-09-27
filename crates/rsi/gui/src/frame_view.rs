//! Immutable block projections retained by one presentation baseline.
use super::{MAX_BYTES, encoded_size};
use crate::projection::Transcript;
use rsi_api_protocol::{ApiError, Result};
use serde::{
    Serialize,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
const NULL_BYTES: usize = b"null".len();

#[derive(Clone, Debug)]
pub(super) struct CachedQueue {
    pub content: CachedValue,
    active: Option<rsi_agent_session_protocol::TurnId>,
}
impl CachedQueue {
    fn capture(live: &Transcript, previous: Option<&Self>) -> Result<Self> {
        let revision = live.queue.revision();
        if let Some(old) = previous.filter(|old| {
            Arc::ptr_eq(&old.content.revision, &revision) && old.active == live.active
        }) {
            return Ok(old.clone());
        }
        let value = serde_json::to_value(live.queue.view(live.active.as_ref()))
            .map_err(|_| ApiError::Capacity)?;
        Ok(CachedQueue {
            content: CachedValue {
                revision,
                bytes: encoded_size(&value)?,
                value: Arc::new(value),
            },
            active: live.active.clone(),
        })
    }
}
impl PartialEq for CachedQueue {
    fn eq(&self, other: &Self) -> bool {
        self.content == other.content
    }
}

#[derive(Clone, Debug)]
pub(super) struct CachedValue {
    revision: Arc<()>,
    pub value: Arc<Value>,
    bytes: usize,
}
impl CachedValue {
    pub fn key(&self) -> &str {
        self.value["key"].as_str().expect("closed block identity")
    }
}
impl PartialEq for CachedValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value) || self.value == other.value
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct CachedTranscript {
    pub metadata: Value,
    pub blocks: Vec<CachedValue>,
    pub turns: CachedValue,
    bytes: usize,
}
#[derive(Debug)]
pub(crate) struct CachedPane {
    pub(super) metadata: Value,
    pub(super) transcript: Option<CachedTranscript>,
    pub(super) queue: Option<CachedQueue>,
    pub(super) bytes: usize,
    #[cfg(test)]
    pub(super) projected_blocks: usize,
    #[cfg(test)]
    pub(super) projected_turns: usize,
}
impl PartialEq for CachedPane {
    fn eq(&self, other: &Self) -> bool {
        self.metadata == other.metadata
            && self.transcript == other.transcript
            && self.queue == other.queue
    }
}
impl CachedPane {
    #[cfg(test)]
    pub(crate) fn capture(
        metadata: Value,
        transcript: Option<&Transcript>,
        previous: Option<&Self>,
    ) -> Result<Self> {
        Self::capture_with_queue(metadata, transcript, None, previous)
    }
    pub(crate) fn capture_with_queue(
        metadata: Value,
        transcript: Option<&Transcript>,
        live: Option<&Transcript>,
        previous: Option<&Self>,
    ) -> Result<Self> {
        let queue = live
            .map(|live| CachedQueue::capture(live, previous.and_then(|pane| pane.queue.as_ref())))
            .transpose()?;
        #[cfg(test)]
        let mut projected = 0;
        #[cfg(test)]
        let mut projected_turns = 0;
        let transcript = transcript
            .map(|transcript| {
                let old_turns = previous
                    .and_then(|pane| pane.transcript.as_ref())
                    .map(|t| &t.turns);
                let previous: BTreeMap<_, _> = previous
                    .and_then(|pane| pane.transcript.as_ref())
                    .into_iter()
                    .flat_map(|transcript| &transcript.blocks)
                    .map(|block| (block.key(), block))
                    .collect();
                let blocks = transcript
                    .blocks
                    .iter()
                    .map(|block| {
                        if let Some(cached) = previous.get(block.key.as_str())
                            && Arc::ptr_eq(&cached.revision, &block.revision)
                        {
                            return Ok((*cached).clone());
                        }
                        #[cfg(test)]
                        {
                            projected += 1;
                        }
                        let value = serde_json::to_value(block).map_err(|_| ApiError::Capacity)?;
                        Ok(CachedValue {
                            revision: block.revision.clone(),
                            bytes: encoded_size(&value)?,
                            value: Arc::new(value),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let turns = if let Some(old) = old_turns
                    .filter(|old| Arc::ptr_eq(&old.revision, &transcript.turns.cache_revision))
                {
                    old.clone()
                } else {
                    #[cfg(test)]
                    {
                        projected_turns += 1;
                    }
                    let value = serde_json::to_value(&transcript.turns.view)
                        .map_err(|_| ApiError::Capacity)?;
                    CachedValue {
                        revision: transcript.turns.cache_revision.clone(),
                        bytes: encoded_size(&value)?,
                        value: Arc::new(value),
                    }
                };
                let metadata = serde_json::to_value(transcript.view(&[] as &[()], ()))
                    .map_err(|_| ApiError::Capacity)?;
                Self::transcript(metadata, blocks, turns)
            })
            .transpose()?;
        let bytes = (encoded_size(&metadata)?
            + queue
                .as_ref()
                .map_or(NULL_BYTES, |queue| queue.content.bytes)
            - NULL_BYTES)
            + transcript
                .as_ref()
                .map_or(0, |transcript| transcript.bytes - NULL_BYTES);
        if bytes > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        Ok(Self {
            metadata,
            transcript,
            queue,
            bytes,
            #[cfg(test)]
            projected_blocks: projected,
            #[cfg(test)]
            projected_turns,
        })
    }
    fn transcript(
        metadata: Value,
        blocks: Vec<CachedValue>,
        turns: CachedValue,
    ) -> Result<CachedTranscript> {
        let bytes = encoded_size(&metadata)? + turns.bytes - NULL_BYTES
            + blocks.iter().map(|block| block.bytes).sum::<usize>()
            + blocks.len().saturating_sub(1);
        if bytes > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        Ok(CachedTranscript {
            metadata,
            blocks,
            turns,
            bytes,
        })
    }
    #[cfg(test)]
    pub(super) fn from_value(mut metadata: Value) -> Result<Self> {
        let transcript = metadata
            .get_mut("transcript")
            .map(|value| {
                let mut value = value.take();
                let blocks = value["blocks"]
                    .take()
                    .as_array()
                    .expect("fixture blocks")
                    .iter()
                    .map(|block| {
                        Ok(CachedValue {
                            revision: Arc::new(()),
                            bytes: encoded_size(block)?,
                            value: Arc::new(block.clone()),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                value["blocks"] = Value::Array(Vec::new());
                let turn_value = value["turns"].take();
                let turns = CachedValue {
                    revision: Arc::new(()),
                    bytes: encoded_size(&turn_value)?,
                    value: Arc::new(turn_value),
                };
                Self::transcript(value, blocks, turns)
            })
            .transpose()?;
        let bytes = encoded_size(&metadata)?
            + transcript
                .as_ref()
                .map_or(0, |value| value.bytes - NULL_BYTES);
        let projected_blocks = transcript.as_ref().map_or(0, |value| value.blocks.len());
        Ok(Self {
            metadata,
            transcript,
            queue: None,
            bytes,
            projected_blocks,
            projected_turns: 1,
        })
    }
}
impl Serialize for CachedPane {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let Some(fields) = self.metadata.as_object() else {
            return self.metadata.serialize(serializer);
        };
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (key, value) in fields {
            if key == "transcript" {
                map.serialize_entry(key, &self.transcript)?;
            } else if key == "queue" && self.queue.is_some() {
                map.serialize_entry(
                    key,
                    self.queue
                        .as_ref()
                        .expect("cached queue")
                        .content
                        .value
                        .as_ref(),
                )?;
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}
impl Serialize for CachedTranscript {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let fields = self.metadata.as_object().expect("closed transcript");
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (key, value) in fields {
            if key == "blocks" {
                map.serialize_entry(key, &Blocks(&self.blocks))?;
            } else if key == "turns" {
                map.serialize_entry(key, self.turns.value.as_ref())?;
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}
struct Blocks<'a>(&'a [CachedValue]);
impl Serialize for Blocks<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut blocks = serializer.serialize_seq(Some(self.0.len()))?;
        for block in self.0 {
            blocks.serialize_element(block.value.as_ref())?;
        }
        blocks.end()
    }
}
