use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionResult, Publication, PublicationObserver, PublicationReceipt,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug, Default)]
struct Observer {
    unconfirmed: AtomicUsize,
    rejected: AtomicUsize,
    hang: bool,
}
#[async_trait]
impl PublicationObserver for Observer {
    async fn committed(&self, _: &PublicationReceipt<'_>) -> ContributionResult<()> {
        panic!("not a publication fixture")
    }
    async fn rejected(&self) -> ContributionResult<()> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        self.rejected.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn unconfirmed(&self) {
        self.unconfirmed.fetch_add(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn rejection_confirms_once_but_dropping_a_pending_notification_preserves_uncertainty() {
    let observer = Arc::new(Observer::default());
    let mut publication = Publication::new(observer.clone());
    publication.rejected().await.unwrap();
    publication.rejected().await.unwrap();
    drop(publication);
    assert_eq!(observer.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(observer.unconfirmed.load(Ordering::SeqCst), 0);

    let observer = Arc::new(Observer {
        hang: true,
        ..Observer::default()
    });
    let mut publication = Publication::new(observer.clone());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), publication.rejected())
            .await
            .is_err()
    );
    drop(publication);
    assert_eq!(observer.rejected.load(Ordering::SeqCst), 0);
    assert_eq!(observer.unconfirmed.load(Ordering::SeqCst), 1);
}
