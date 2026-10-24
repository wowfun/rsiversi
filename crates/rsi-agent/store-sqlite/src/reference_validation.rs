//! Store-generation coverage proof advanced by complete, tracked writes.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    changes: u64,
    external: i64,
}
impl Stamp {
    pub(super) fn read(owner: &StoreInner, connection: &Connection) -> Result<Self> {
        Ok(Self {
            changes: owner.reference_commits.load(Ordering::Relaxed),
            external: connection
                .query_row("PRAGMA data_version", [], |row| row.get(0))
                .map_err(sql_error)?,
        })
    }
}
pub(super) struct Proof {
    stamp: Stamp,
    pub(super) strict: bool,
}
pub(super) fn check_cached(owner: &StoreInner, connection: &Connection) -> Result<()> {
    let mut proof = owner.reference_proof.lock().expect("reference proof");
    if let Some(current) = proof.as_ref()
        && current.stamp != Stamp::read(owner, connection)?
    {
        *proof = None;
    }
    Ok(())
}
pub(super) fn after_write(
    owner: &StoreInner,
    connection: &Connection,
    success: bool,
) -> Result<()> {
    let mut proof = owner.reference_proof.lock().expect("reference proof");
    if let Some(current) = proof.as_mut() {
        let after = Stamp::read(owner, connection)?;
        if current.stamp.external != after.external
            || (current.stamp.changes != after.changes
                && (!success
                    || (!current.strict
                        && connection
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM reference_records WHERE complete=0)",
                                [],
                                |row| row.get::<_, bool>(0),
                            )
                            .map_err(sql_error)?)))
        {
            *proof = None;
        } else {
            current.stamp = after;
        }
    }
    Ok(())
}
pub(super) fn strict(owner: &StoreInner) {
    if let Some(proof) = owner
        .reference_proof
        .lock()
        .expect("reference proof")
        .as_mut()
    {
        proof.strict = true;
    }
}

/// The immediate transaction excludes foreign commits between proof and admission.
pub(super) fn ensure(owner: &StoreInner, transaction: &Transaction<'_>) -> Result<()> {
    check_cached(owner, transaction)?;
    if owner
        .reference_proof
        .lock()
        .expect("reference proof")
        .is_some()
    {
        return Ok(());
    }
    #[cfg(test)]
    owner.reference_audits.fetch_add(1, Ordering::Relaxed);
    reference_index::validate_all(transaction, &owner.reference_codecs)?;
    let stamp = Stamp::read(owner, transaction)?;
    let strict = transaction
        .query_row(
            "SELECT COALESCE((SELECT strict FROM reference_policy WHERE id=1),0)",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    *owner.reference_proof.lock().expect("reference proof") = Some(Proof { stamp, strict });
    Ok(())
}

impl SqliteStore {
    pub(super) async fn validate_references(&self) -> Result<()> {
        let owner = self.inner.clone();
        self.with_writer(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            ensure(&owner, &transaction)?;
            // This audit changes no rows. End its snapshot without a write commit.
            drop(transaction);
            Ok(())
        })
        .await
    }
}
