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

#[derive(Clone, Debug)]
pub(super) struct CachedBlock {
    revision: Arc<()>,
    pub value: Arc<Value>,
    bytes: usize,
}
impl CachedBlock {
    pub fn key(&self) -> &str {
        self.value["key"].as_str().expect("closed block identity")
    }
}
impl PartialEq for CachedBlock {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value) || self.value == other.value
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct CachedTranscript {
    pub metadata: Value,
    pub blocks: Vec<CachedBlock>,
    bytes: usize,
}
#[derive(Debug)]
pub(crate) struct CachedPane {
    pub(super) metadata: Value,
    pub(super) transcript: Option<CachedTranscript>,
    pub(super) bytes: usize,
    #[cfg(test)]
    pub(super) projected_blocks: usize,
}
impl PartialEq for CachedPane {
    fn eq(&self, other: &Self) -> bool {
        self.metadata == other.metadata && self.transcript == other.transcript
    }
}
impl CachedPane {
    pub(crate) fn capture(
        metadata: Value,
        transcript: Option<&Transcript>,
        previous: Option<&Self>,
    ) -> Result<Self> {
        #[cfg(test)]
        let mut projected = 0;
        let transcript = transcript
            .map(|transcript| {
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
                        Ok(CachedBlock {
                            revision: block.revision.clone(),
                            bytes: encoded_size(&value)?,
                            value: Arc::new(value),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let metadata = serde_json::to_value(transcript.view(&[] as &[()]))
                    .map_err(|_| ApiError::Capacity)?;
                Self::transcript(metadata, blocks)
            })
            .transpose()?;
        let bytes = encoded_size(&metadata)?
            + transcript
                .as_ref()
                .map_or(0, |transcript| transcript.bytes - 4);
        if bytes > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        Ok(Self {
            metadata,
            transcript,
            bytes,
            #[cfg(test)]
            projected_blocks: projected,
        })
    }
    fn transcript(metadata: Value, blocks: Vec<CachedBlock>) -> Result<CachedTranscript> {
        let bytes = encoded_size(&metadata)?
            + blocks.iter().map(|block| block.bytes).sum::<usize>()
            + blocks.len().saturating_sub(1);
        if bytes > MAX_BYTES {
            return Err(ApiError::Capacity);
        }
        Ok(CachedTranscript {
            metadata,
            blocks,
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
                        Ok(CachedBlock {
                            revision: Arc::new(()),
                            bytes: encoded_size(block)?,
                            value: Arc::new(block.clone()),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                value["blocks"] = Value::Array(Vec::new());
                Self::transcript(value, blocks)
            })
            .transpose()?;
        let bytes =
            encoded_size(&metadata)? + transcript.as_ref().map_or(0, |value| value.bytes - 4);
        let projected_blocks = transcript.as_ref().map_or(0, |value| value.blocks.len());
        Ok(Self {
            metadata,
            transcript,
            bytes,
            projected_blocks,
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
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}
struct Blocks<'a>(&'a [CachedBlock]);
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
