//! Bounded independent launch tickets for one resident browser device.
use rsi_api_protocol::{ApiError, Result};
use rsi_credentials_protocol::SecretValue;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, sync::Mutex, time::Duration};

const MAX_TICKETS: usize = 64;
pub(super) struct Tickets {
    execution: rsi_meta::Execution,
    pending: Mutex<BTreeMap<[u8; 32], (rsi_meta::Deadline, SecretValue)>>,
}
impl std::fmt::Debug for Tickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserLaunchTickets")
            .finish_non_exhaustive()
    }
}
impl Tickets {
    pub fn new(execution: rsi_meta::Execution) -> Self {
        Self {
            execution,
            pending: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn mint(&self, credential: SecretValue) -> Result<SecretValue> {
        let mut pending = self.pending.lock().expect("launch tickets");
        pending.retain(|_, (expiry, _)| !expiry.has_elapsed());
        if pending.len() >= MAX_TICKETS {
            return Err(ApiError::Invalid(
                "outstanding browser launch ticket capacity reached".into(),
            ));
        }
        let mut entropy = [0; 32];
        getrandom::fill(&mut entropy)
            .map_err(|_| ApiError::Backend("launch entropy unavailable".into()))?;
        let ticket = SecretValue::new(hex::encode(entropy))
            .map_err(|_| ApiError::Backend("launch ticket unavailable".into()))?;
        pending.insert(
            Sha256::digest(ticket.expose_secret().as_bytes()).into(),
            (
                self.execution.deadline_after(Duration::from_mins(10)),
                credential,
            ),
        );
        Ok(ticket)
    }
    pub fn clear(&self) {
        self.pending.lock().expect("launch tickets").clear();
    }
}
impl rsi_api_http::BrowserBootstrap for Tickets {
    fn redeem(&self, ticket: &SecretValue) -> Result<SecretValue> {
        let mut pending = self.pending.lock().expect("launch tickets");
        pending.retain(|_, (expiry, _)| !expiry.has_elapsed());
        let digest: [u8; 32] = Sha256::digest(ticket.expose_secret().as_bytes()).into();
        pending
            .remove(&digest)
            .map(|(_, credential)| credential)
            .ok_or(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_http::BrowserBootstrap as _;
    #[tokio::test(start_paused = true)]
    async fn repeated_launches_are_independent_one_use_bounded_and_expiring() {
        let tickets = Tickets::new(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        ));
        let credential = SecretValue::new("same-browser-device").unwrap();
        let first = tickets.mint(credential.clone()).unwrap();
        let second = tickets.mint(credential.clone()).unwrap();
        assert_ne!(first.expose_secret(), second.expose_secret());
        assert_eq!(
            tickets.redeem(&first).unwrap().expose_secret(),
            credential.expose_secret()
        );
        assert!(tickets.redeem(&first).is_err());
        assert_eq!(
            tickets.redeem(&second).unwrap().expose_secret(),
            credential.expose_secret()
        );
        for _ in 0..MAX_TICKETS {
            tickets.mint(credential.clone()).unwrap();
        }
        assert!(tickets.mint(credential.clone()).is_err());
        tokio::time::advance(Duration::from_secs(601)).await;
        let fresh = tickets.mint(credential).unwrap();
        tickets.clear();
        assert!(tickets.redeem(&fresh).is_err());
    }
}
