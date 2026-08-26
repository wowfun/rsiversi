use crate::{
    BytesOutput, CapId, CapOutput, OutputPrefix, PrepareOutput, RawBytes, RawRequirement,
    ReleaseId, ServiceRequirement,
};

pub(super) struct OwnedRequirement {
    key: Box<[u8]>,
    contract: Box<[u8]>,
}

impl OwnedRequirement {
    fn new(requirement: &ServiceRequirement) -> Self {
        Self {
            key: requirement.key.as_bytes().into(),
            contract: requirement.contract.as_bytes().into(),
        }
    }
}

pub(super) enum OutputPayload {
    None,
    Bytes(Box<[u8]>),
    Prepare {
        normalized: Box<[u8]>,
        _storage: Box<[OwnedRequirement]>,
        raw: Box<[RawRequirement]>,
        prepared: CapId,
        retained_bytes: u64,
    },
    Capability(CapId),
}

pub(super) struct OutputRecord {
    diagnostic: Box<[u8]>,
    pub(super) held_capabilities: Vec<CapId>,
    payload: OutputPayload,
}

impl OutputRecord {
    pub(super) fn diagnostic(text: String) -> Self {
        Self {
            diagnostic: bounded(text).into_bytes().into(),
            held_capabilities: Vec::new(),
            payload: OutputPayload::None,
        }
    }

    pub(super) fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            diagnostic: Box::default(),
            held_capabilities: Vec::new(),
            payload: OutputPayload::Bytes(bytes.into()),
        }
    }

    pub(super) fn capability(capability: CapId) -> Self {
        Self {
            diagnostic: Box::default(),
            held_capabilities: vec![capability],
            payload: OutputPayload::Capability(capability),
        }
    }

    pub(super) fn prepare(
        normalized: Vec<u8>,
        requirements: &[ServiceRequirement],
        prepared: CapId,
        retained_bytes: u64,
    ) -> Self {
        let owned: Box<[_]> = requirements.iter().map(OwnedRequirement::new).collect();
        let raw = owned
            .iter()
            .zip(requirements)
            .map(|(storage, requirement)| RawRequirement {
                key: raw_bytes(&storage.key),
                contract: raw_bytes(&storage.contract),
                version: requirement.version,
            })
            .collect();
        Self {
            diagnostic: Box::default(),
            held_capabilities: vec![prepared],
            payload: OutputPayload::Prepare {
                normalized: normalized.into(),
                _storage: owned,
                raw,
                prepared,
                retained_bytes,
            },
        }
    }

    pub(super) fn prefix(&self, release: ReleaseId, struct_size: u32) -> OutputPrefix {
        OutputPrefix {
            struct_size,
            reserved: 0,
            release,
            diagnostic: raw_bytes(&self.diagnostic),
        }
    }

    pub(super) fn bytes_output(&self, release: ReleaseId) -> BytesOutput {
        let OutputPayload::Bytes(bytes) = &self.payload else {
            unreachable!("bytes output record has bytes payload")
        };
        BytesOutput {
            prefix: self.prefix(release, size_u32::<BytesOutput>()),
            bytes: raw_bytes(bytes),
        }
    }

    pub(super) fn cap_output(&self, release: ReleaseId) -> CapOutput {
        let OutputPayload::Capability(capability) = self.payload else {
            unreachable!("cap output record has capability payload")
        };
        CapOutput {
            prefix: self.prefix(release, size_u32::<CapOutput>()),
            capability,
        }
    }

    pub(super) fn prepare_output(&self, release: ReleaseId) -> PrepareOutput {
        let OutputPayload::Prepare {
            normalized,
            raw,
            prepared,
            retained_bytes,
            ..
        } = &self.payload
        else {
            unreachable!("prepare output record has prepare payload")
        };
        PrepareOutput {
            prefix: self.prefix(release, size_u32::<PrepareOutput>()),
            prepared: *prepared,
            normalized_config: raw_bytes(normalized),
            requirements: raw.as_ptr(),
            requirement_count: u64::try_from(raw.len()).expect("bounded requirement count"),
            retained_bytes: *retained_bytes,
        }
    }
}

pub(super) fn raw_bytes(bytes: &[u8]) -> RawBytes {
    RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).expect("bounded native output"),
    }
}

pub(super) fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("ABI output type exceeds u32")
}

fn bounded(mut diagnostic: String) -> String {
    const LIMIT: usize = 4_096;
    if diagnostic.len() > LIMIT {
        let mut end = LIMIT;
        while !diagnostic.is_char_boundary(end) {
            end -= 1;
        }
        diagnostic.truncate(end);
    }
    diagnostic
}
