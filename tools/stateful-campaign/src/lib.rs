//! Deterministic stateful randomized (model-based) hostility campaign
//! against the Signal Fish polling driver + shared `ClientCore`.
//!
//! Every finding reproduces from `(seed, script index, policy)`. The crate is
//! a workspace member so the mandatory clippy/test gates cover it; the binary
//! drives long campaigns from CI and the library exposes canaries and a
//! reduced smoke campaign as unit tests.

pub mod canary;
pub mod gen;
pub mod rng;
pub mod run;
pub mod script;
pub mod soak;
pub mod transport;

#[cfg(test)]
mod tests {
    use super::canary;
    use super::gen::generate_script;
    use super::run::run_script;
    use signal_fish_client::client::ProtocolViolationPolicy;

    const ALL_POLICIES: [ProtocolViolationPolicy; 3] = [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Disconnect,
    ];

    #[test]
    fn oracle_canaries_pass() {
        assert_eq!(
            canary::selftest(),
            0,
            "oracle canaries detect known-good and known-bad streams"
        );
    }

    #[test]
    fn smoke_campaign_is_clean() {
        // Deterministic reduced campaign: 2 seeds x 12 scripts x 3 policies.
        for seed in 1..=2u64 {
            for index in 0..12usize {
                let script = generate_script(seed, index);
                for policy in ALL_POLICIES {
                    let outcome = run_script(&script, policy);
                    let details = outcome.as_ref().err().map(|failure| {
                        failure
                            .findings
                            .iter()
                            .map(|finding| format!("{}: {}", finding.category, finding.detail))
                            .collect::<Vec<String>>()
                    });
                    assert!(
                        outcome.is_ok(),
                        "seed={seed} script={index} archetype={} policy={policy:?}: {details:?}",
                        script.archetype
                    );
                    if let Ok(outcome) = outcome {
                        assert!(
                            outcome.frames_fed > 0,
                            "seed={seed} script={index} fed no frames"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn soak_probe_is_clean() {
        assert_eq!(super::soak::soak_probe(), 0, "long-horizon churn probe");
    }
}
