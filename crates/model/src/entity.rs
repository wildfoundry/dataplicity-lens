use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessId(pub u32);

impl std::fmt::Display for ProcessId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EntityId {
    Host(String),
    Finding(String),
    Process { pid: ProcessId, start_ticks: u64 },
    User(u32),
    Cgroup(String),
    Service(String),
    Container(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub uid: u32,
    pub name: Option<String>,
}

impl User {
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.uid.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cgroup {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReference {
    pub name: String,
    pub inferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerReference {
    pub runtime: Option<String>,
    pub id: String,
    pub inferred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    ParentProcess,
    ChildProcess,
    OwnedByUser,
    MemberOfCgroup,
    MemberOfService,
    MemberOfContainer,
    FindingConcerns,
    FindingOnHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relationship {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: RelationshipKind,
}
