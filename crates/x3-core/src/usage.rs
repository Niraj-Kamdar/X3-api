use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{PricingUnit, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageStatus {
    Ok,
    Error,
}

impl UsageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageStatus::Ok => "ok",
            UsageStatus::Error => "error",
        }
    }
}

/// One logged call. Never contains request content or secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub ts: DateTime<Utc>,
    pub token_id: String,
    pub token_label: String,
    pub model: String,
    pub provider: String,
    pub task: Task,
    pub units: f64,
    pub unit: PricingUnit,
    pub est_cost_usd: f64,
    pub status: UsageStatus,
}
