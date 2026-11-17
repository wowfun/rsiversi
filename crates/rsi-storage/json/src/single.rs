use super::{
    Address, BTreeMap, Deserialize, EncodedValue, JSON_STORAGE_FORMAT, MAXIMUM_STORAGE_RECORDS,
    Mutation, Path, RecordObjectSize, Serialize, StorageError, UnitOwner, UnitSnapshot, UnitState,
    check_size, corrupt, read_optional, sync_parent_directory,
};
use serde::{
    de::{SeqAccess, Visitor},
    ser::SerializeSeq,
};
use serde_json::value::RawValue;

#[derive(Serialize)]
pub(super) struct Document<'a> {
    format: u32,
    unit: &'a str,
    version: u32,
    #[serde(serialize_with = "serialize_records")]
    records: &'a BTreeMap<Address, EncodedValue>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input<'a> {
    format: u32,
    unit: String,
    version: u32,
    #[serde(borrow)]
    records: Records<'a>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record<'a> {
    address: Address,
    #[serde(borrow)]
    value: &'a RawValue,
}
struct Records<'a>(Vec<Record<'a>>);
impl<'de: 'a, 'a> Deserialize<'de> for Records<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Bounded;
        impl<'de> Visitor<'de> for Bounded {
            type Value = Records<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded records")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut records = Vec::new();
                while let Some(record) = seq.next_element()? {
                    if records.len() == MAXIMUM_STORAGE_RECORDS {
                        return Err(serde::de::Error::custom("record count exceeds bound"));
                    }
                    records.push(record);
                }
                Ok(Records(records))
            }
        }
        d.deserialize_seq(Bounded)
    }
}
fn serialize_records<S: serde::Serializer>(
    records: &BTreeMap<Address, EncodedValue>,
    s: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Entry<'a> {
        address: &'a Address,
        value: &'a EncodedValue,
    }
    let mut seq = s.serialize_seq(Some(records.len()))?;
    for (address, value) in records {
        seq.serialize_element(&Entry { address, value })?;
    }
    seq.end()
}
pub(super) fn load(
    owner: &UnitOwner,
    path: &Path,
) -> Result<(UnitSnapshot, UnitState), StorageError> {
    let mut snapshot = UnitSnapshot::default();
    let mut size = RecordObjectSize::default();
    if let Some(bytes) = read_optional(path, owner.backend.config.maximum_document_bytes)? {
        let input: Input<'_> = serde_json::from_slice(&bytes).map_err(corrupt)?;
        if input.format != JSON_STORAGE_FORMAT {
            return Err(corrupt("unsupported JSON storage format"));
        }
        if input.unit != owner.descriptor.name() {
            return Err(corrupt("JSON unit identity mismatch"));
        }
        owner.descriptor.admit_version(input.version)?;
        snapshot.version = Some(input.version);
        for record in input.records.0 {
            owner
                .descriptor
                .admit_address(&record.address)
                .map_err(corrupt)?;
            let value = EncodedValue::copy_raw(record.value).map_err(corrupt)?;
            size = size
                .with_entry(None, record.address.entry_bytes(value.encoded_len())?)
                .map_err(corrupt)?;
            check_size(size).map_err(corrupt)?;
            if snapshot.records.insert(record.address, value).is_some() {
                return Err(corrupt("duplicate stored address"));
            }
        }
        sync_parent_directory(path.parent().expect("unit path has backend root"))?;
    }
    let state = UnitState::Single {
        path: path.to_owned(),
        records: snapshot.records.clone(),
        size,
    };
    Ok((snapshot, state))
}
pub(super) fn write<P>(
    owner: &UnitOwner,
    path: &Path,
    current: &mut BTreeMap<Address, EncodedValue>,
    size: &mut RecordObjectSize,
    mut changes: Vec<Mutation>,
    persist: P,
) -> Result<(), StorageError>
where
    P: FnOnce(&Path, usize, &Document<'_>) -> Result<(), StorageError>,
{
    if changes
        .iter()
        .all(|change| change.value.is_none() && !current.contains_key(&change.address))
    {
        return Ok(());
    }
    let mut next = *size;
    for change in &changes {
        let old = current
            .get(&change.address)
            .map(|value| change.address.entry_bytes(value.encoded_len()))
            .transpose()?;
        if let Some(value) = &change.value {
            next = next.with_entry(old, change.address.entry_bytes(value.encoded_len())?)?;
        } else if let Some(old) = old {
            next = next.without_entry(old)?;
        }
    }
    check_size(next)?;
    // Reuse the bounded mutation vector as an undo log; untouched entries stay put.
    for change in &mut changes {
        change.value = match change.value.take() {
            Some(value) => current.insert(change.address.clone(), value),
            None => current.remove(&change.address),
        };
    }
    let result = persist(
        path,
        owner.backend.config.maximum_document_bytes,
        &Document {
            format: JSON_STORAGE_FORMAT,
            unit: owner.descriptor.name(),
            version: owner.descriptor.version(),
            records: current,
        },
    );
    if result.is_ok() {
        *size = next;
    } else {
        for change in changes {
            if let Some(value) = change.value {
                current.insert(change.address, value);
            } else {
                current.remove(&change.address);
            }
        }
    }
    result
}
