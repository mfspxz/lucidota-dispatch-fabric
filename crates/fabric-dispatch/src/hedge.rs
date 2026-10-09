//! T065: hedged (duplicate) inference.
//!
//! For explicitly eligible tasks, a second inference request may be launched
//! after `hedge_delay_ms` if the primary has not finished. The first
//! *successful* response wins; if both fail, the primary's error is returned
//! so diagnostics stay anchored to the first attempt. Hedging requires both
//! policy permission and quota headroom — it never fires speculatively.

use fabric_types::{FabricError, Result, TaskClass, TaskContract};
use std::future::Future;

/// Policy governing hedged inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HedgePolicy {
    pub enabled: bool,
    /// Task classes allowed to hedge. Unknown classes never match.
    pub allowed_classes: Vec<TaskClass>,
    /// Extra duplicate requests beyond the primary (1..=3).
    pub max_duplicates: u32,
    /// Delay after primary launch before the hedge fires.
    pub hedge_delay_ms: u64,
}

impl HedgePolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed_classes: vec![],
            max_duplicates: 0,
            hedge_delay_ms: 0,
        }
    }

    pub fn new(
        enabled: bool,
        allowed_classes: Vec<TaskClass>,
        max_duplicates: u32,
        hedge_delay_ms: u64,
    ) -> Result<Self> {
        if enabled {
            if !(1..=3).contains(&max_duplicates) {
                return Err(FabricError::InvalidInput {
                    field: "max_duplicates",
                    reason: "must be 1..=3 when hedging is enabled".to_string(),
                });
            }
            if hedge_delay_ms == 0 {
                return Err(FabricError::InvalidInput {
                    field: "hedge_delay_ms",
                    reason: "must be positive when hedging is enabled".to_string(),
                });
            }
        }
        Ok(Self {
            enabled,
            allowed_classes,
            max_duplicates,
            hedge_delay_ms,
        })
    }
}

/// Explicit eligibility check: hedging fires only when the policy enables it,
/// the task's class is allow-listed, and the caller confirms quota headroom.
/// Unknown task classes are never eligible.
pub fn hedge_eligible(
    policy: &HedgePolicy,
    contract: &TaskContract,
    quota_ok: bool,
) -> bool {
    policy.enabled
        && quota_ok
        && policy.max_duplicates >= 1
        && policy.allowed_classes.contains(&contract.class)
}

/// Race a primary future against one hedge launched after `hedge_delay_ms`.
///
/// - If the primary completes before the delay, the hedge never launches and
///   the primary's result is returned.
/// - Otherwise both run; the first `Ok` wins.
/// - If both fail, the primary's error is returned.
/// - If the primary fails first but the hedge is still running, the hedge is
///   awaited: a late success still beats an early failure.
pub async fn race_primary_hedge<F1, F2, T, E>(
    primary: F1,
    hedge: F2,
    hedge_delay_ms: u64,
) -> std::result::Result<T, E>
where
    F1: Future<Output = std::result::Result<T, E>>,
    F2: Future<Output = std::result::Result<T, E>>,
{
    use std::pin::pin;
    let mut primary = pin!(primary);
    let delay = tokio::time::sleep(std::time::Duration::from_millis(hedge_delay_ms));

    // Phase 1: primary alone until the hedge delay expires.
    let hedge_fut = tokio::select! {
        biased;
        r = &mut primary => return r,
        _ = delay => hedge,
    };
    let mut hedge_fut = pin!(hedge_fut);

    // Phase 2: both in flight; first success wins. The loop-top check keeps
    // select! from ever running with all branches disabled.
    let mut primary_err: Option<E> = None;
    let mut hedge_done = false;
    loop {
        if hedge_done {
            // Hedge is over; only the primary can still change the outcome.
            return match primary_err.take() {
                Some(e) => Err(e),     // primary already failed
                None => primary.await, // wait for the primary alone
            };
        }
        tokio::select! {
            biased;
            r = &mut primary, if primary_err.is_none() => {
                match r {
                    ok @ Ok(_) => return ok,
                    Err(e) => primary_err = Some(e),
                }
            }
            r = &mut hedge_fut => {
                hedge_done = true;
                match r {
                    ok @ Ok(_) => return ok,
                    Err(_) => {
                        if let Some(e) = primary_err.take() {
                            return Err(e);
                        }
                        // Hedge failed but the primary is still running:
                        // loop top waits for the primary alone.
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric_types::{AcceptancePredicate, Scope, TaskId};

    fn contract_with(class: TaskClass) -> TaskContract {
        TaskContract::builder(TaskId::new("t1").unwrap(), class, "work")
            .spec_revision("rev")
            .inputs(vec!["in".to_string()])
            .outputs(vec!["out".to_string()])
            .scope(Scope::new(vec!["x".to_string()]).unwrap())
            .acceptance(vec![AcceptancePredicate::new("p", "d", true).unwrap()])
            .verifiers(vec!["v".to_string()])
            .build()
            .unwrap()
    }

    #[test]
    fn policy_validation() {
        assert!(HedgePolicy::new(true, vec![TaskClass::Implement], 0, 100).is_err());
        assert!(HedgePolicy::new(true, vec![TaskClass::Implement], 4, 100).is_err());
        assert!(HedgePolicy::new(true, vec![TaskClass::Implement], 1, 0).is_err());
        assert!(HedgePolicy::new(true, vec![TaskClass::Implement], 2, 100).is_ok());
        assert!(!HedgePolicy::disabled().enabled);
    }

    #[test]
    fn eligibility_requires_policy_class_and_quota() {
        let p = HedgePolicy::new(true, vec![TaskClass::Implement], 1, 100).unwrap();
        let ok = contract_with(TaskClass::Implement);
        assert!(hedge_eligible(&p, &ok, true));
        assert!(!hedge_eligible(&p, &ok, false)); // no quota headroom
        assert!(!hedge_eligible(&p, &contract_with(TaskClass::Review), true)); // class not listed
        assert!(!hedge_eligible(&p, &contract_with(TaskClass::Unknown("X".into())), true));
        assert!(!hedge_eligible(&HedgePolicy::disabled(), &ok, true));
    }

    #[tokio::test]
    async fn fast_primary_never_launches_hedge() {
        let r = race_primary_hedge(
            async { Ok::<_, &str>(1) },
            async {
                panic!("hedge must not run");
                #[allow(unreachable_code)]
                Ok::<_, &str>(2)
            },
            1000,
        )
        .await;
        assert_eq!(r, Ok(1));
    }

    #[tokio::test]
    async fn hedge_wins_when_primary_stalls() {
        let r = race_primary_hedge(
            async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok::<_, &str>(1)
            },
            async { Ok::<_, &str>(2) },
            50,
        )
        .await;
        assert_eq!(r, Ok(2));
    }

    #[tokio::test]
    async fn primary_failure_still_allows_hedge_success() {
        let r = race_primary_hedge(
            async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Err::<i32, &str>("primary boom")
            },
            async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                Ok::<i32, &str>(7)
            },
            50,
        )
        .await;
        assert_eq!(r, Ok(7));
    }

    #[tokio::test]
    async fn both_failing_returns_primary_error() {
        let r: std::result::Result<i32, &str> = race_primary_hedge(
            async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Err("primary boom")
            },
            async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                Err("hedge boom")
            },
            50,
        )
        .await;
        assert_eq!(r, Err("primary boom"));
    }
}
