use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// What a budget applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "lowercase")]
pub enum BudgetScope {
    /// Monthly ceiling for a cloud provider, across all tokens.
    Provider(String),
    /// Lifetime spend cap of one local API token (identified by label).
    Token(String),
}

impl fmt::Display for BudgetScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BudgetScope::Provider(p) => write!(f, "provider '{p}' (monthly)"),
            BudgetScope::Token(t) => write!(f, "token '{t}'"),
        }
    }
}

/// Provider-level ceilings. Token caps live on the token itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Budgets {
    /// Monthly (calendar month, UTC) spend ceiling per provider, in USD.
    /// Providers without an entry are uncapped.
    #[serde(default)]
    pub provider_monthly_usd: BTreeMap<String, f64>,
}

/// Inputs for a pre-flight budget check.
#[derive(Debug, Clone)]
pub struct BudgetCheck<'a> {
    pub provider: &'a str,
    pub provider_spent_this_month: f64,
    pub token_label: &'a str,
    pub token_cap: Option<f64>,
    pub token_spent: f64,
    pub estimate: f64,
}

impl Budgets {
    /// Reject the request if the estimated cost would push either the
    /// provider's monthly ceiling or the token's cap over the limit.
    /// Free requests (estimate == 0) always pass.
    pub fn check(&self, c: &BudgetCheck<'_>) -> Result<()> {
        if c.estimate <= 0.0 {
            return Ok(());
        }
        if let Some(&cap) = self.provider_monthly_usd.get(c.provider) {
            if c.provider_spent_this_month + c.estimate > cap {
                return Err(Error::BudgetExceeded {
                    scope: BudgetScope::Provider(c.provider.to_string()),
                    spent: c.provider_spent_this_month,
                    estimate: c.estimate,
                    cap,
                });
            }
        }
        if let Some(cap) = c.token_cap {
            if c.token_spent + c.estimate > cap {
                return Err(Error::BudgetExceeded {
                    scope: BudgetScope::Token(c.token_label.to_string()),
                    spent: c.token_spent,
                    estimate: c.estimate,
                    cap,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(
        provider_spent: f64,
        token_cap: Option<f64>,
        token_spent: f64,
        est: f64,
    ) -> Result<()> {
        let mut b = Budgets::default();
        b.provider_monthly_usd.insert("elevenlabs".into(), 5.0);
        b.check(&BudgetCheck {
            provider: "elevenlabs",
            provider_spent_this_month: provider_spent,
            token_label: "claude",
            token_cap,
            token_spent,
            estimate: est,
        })
    }

    #[test]
    fn provider_and_token_caps() {
        assert!(check(4.0, None, 0.0, 0.5).is_ok());
        assert!(matches!(
            check(4.9, None, 0.0, 0.5),
            Err(Error::BudgetExceeded {
                scope: BudgetScope::Provider(_),
                ..
            })
        ));
        assert!(matches!(
            check(0.0, Some(1.0), 0.9, 0.2),
            Err(Error::BudgetExceeded {
                scope: BudgetScope::Token(_),
                ..
            })
        ));
        // free calls always pass
        assert!(check(100.0, Some(0.0), 100.0, 0.0).is_ok());
    }
}
