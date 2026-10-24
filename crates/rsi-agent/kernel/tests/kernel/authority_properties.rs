use super::*;
use rsi_agent_session_protocol::{DomainIdentity, DomainRequestId, DomainRevision};
use rsi_agent_turn_protocol::{ContinuationBinding, ContinuationIssuer, ResumeAdmissionIssuer};
#[test]
fn generated_resume_issuers_preserve_exact_header_and_reject_foreign_authority() {
    for index in 0..512 {
        let foreign = index % 2 == 0;
        let issuer = ResumeAdmissionIssuer::new();
        let other = ResumeAdmissionIssuer::new();
        let header = header(&format!("resume-{index}"));
        let pin = test_pin(header.agent_preset_id());
        let token = issuer.issue(header.clone(), pin.clone()).unwrap();
        assert!(matches!(other.inspect(&token), Err(TurnError::Invalid(_))));
        let same = issuer.clone();
        let (actual, generation) = same.inspect(&token).unwrap();
        assert_eq!(actual, &header);
        assert!(generation.same_generation(&pin));
        if foreign {
            assert!(matches!(other.consume(token), Err(TurnError::Invalid(_))));
        } else {
            let (actual, generation) = issuer.consume(token).unwrap();
            assert_eq!(actual, header);
            assert!(generation.same_generation(&pin));
        }
    }
}
#[test]
fn generated_continuation_sequences_never_rearm_or_regress() {
    let mut random = 0x525349_u64;
    for _case in 0..256 {
        let issuer = ContinuationIssuer::default();
        let foreign = ContinuationIssuer::default();
        let header = header("continuation-property");
        let pin = test_pin(header.agent_preset_id());
        let binding = ContinuationBinding {
            domain: DomainIdentity::new("property", 1).unwrap(),
            owner: DomainRequestId::new("owner").unwrap(),
            revision: DomainRevision::new(0),
            snapshot_sha256: "a".repeat(64),
        };
        let lease = issuer.issue(header, pin, binding, None);
        let weak = lease.downgrade();
        let cancelled = lease.disarmed_token();
        let mut maximum = 0_u64;
        let mut live = true;
        for _step in 0..128 {
            random = random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let operation = (random >> 32) % 5;
            let value = ((random >> 40) & 2047) as u16;
            match operation {
                0 => {
                    issuer
                        .set_revision(&lease, DomainRevision::new(value.into()))
                        .unwrap();
                    maximum = maximum.max(value.into());
                }
                1 => {
                    lease.revoke();
                    live = false;
                }
                2 => {
                    assert_eq!(issuer.request_round(&lease).is_ok(), live);
                }
                3 => {
                    assert!(matches!(
                        foreign.set_revision(&lease, DomainRevision::new(value.into())),
                        Err(TurnError::Invalid(_))
                    ));
                }
                _ => {
                    issuer.admitted_round(&lease, value.into()).unwrap();
                    assert!(lease.demand().is_none());
                }
            }
            assert_eq!(lease.revision().get(), maximum);
            assert_eq!(lease.is_armed(), live);
            assert!(weak.upgrade().is_some());
        }
        drop(lease);
        assert!(weak.upgrade().is_none());
        assert!(cancelled.is_cancelled());
    }
}
