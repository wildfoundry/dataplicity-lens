use serde::{Deserialize, Serialize};

use crate::EntityId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Information,
    Attention,
    Critical,
}

impl Severity {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Information => "information",
            Self::Attention => "attention",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub label: String,
    pub value: String,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    pub evidence: Vec<Evidence>,
    pub related_entities: Vec<EntityId>,
    pub suggested_actions: Vec<String>,
}
