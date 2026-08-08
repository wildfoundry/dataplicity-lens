#![forbid(unsafe_code)]

mod scripting;

pub use scripting::{
    AssertionError, AssertionPolicy, EXIT_ASSERTION, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE,
    FailOnSeverity, MatchMode, PROJECTABLE_FIELDS, PrimaryDomain, UsageError, exit_code_from_error,
    parse_fields_list, project_snapshot_value,
};

use std::{cmp::Ordering, collections::HashMap};

use lens_model::{Process, ProcessId, ProcessState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Cpu,
    Memory,
    Pid,
    Name,
    User,
    Runtime,
    ReadRate,
    WriteRate,
    Threads,
}

impl SortKey {
    pub const ALL: [Self; 9] = [
        Self::Cpu,
        Self::Memory,
        Self::Pid,
        Self::Name,
        Self::User,
        Self::Runtime,
        Self::ReadRate,
        Self::WriteRate,
        Self::Threads,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Pid => "PID",
            Self::Name => "Name",
            Self::User => "User",
            Self::Runtime => "Runtime",
            Self::ReadRate => "Read rate",
            Self::WriteRate => "Write rate",
            Self::Threads => "Threads",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    #[default]
    None,
    Tree,
    User,
    Service,
}

impl GroupMode {
    pub const ALL: [Self; 4] = [Self::None, Self::Tree, Self::User, Self::Service];

    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tree => "process tree",
            Self::User => "user",
            Self::Service => "service/cgroup",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::None => Self::Tree,
            Self::Tree => Self::User,
            Self::User => Self::Service,
            Self::Service => Self::None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessFilter {
    pub search: Option<String>,
    pub user: Option<String>,
    pub state: Option<ProcessState>,
    pub min_cpu: Option<f64>,
    pub min_memory: Option<f64>,
    pub name: Option<String>,
    pub exact_name: Option<String>,
    pub service_or_cgroup: Option<String>,
    pub cgroup: Option<String>,
    pub pid: Option<u32>,
    pub ppid: Option<u32>,
    #[serde(default)]
    pub match_mode: MatchMode,
}

impl ProcessFilter {
    pub fn matches(&self, process: &Process) -> bool {
        let mode = self.match_mode;
        if let Some(search) = normalized(&self.search) {
            let pid = process.pid.0.to_string();
            let haystacks = [
                process.name.as_str(),
                process.command_line.as_deref().unwrap_or_default(),
                process.user.name.as_deref().unwrap_or_default(),
                process
                    .service
                    .as_ref()
                    .map_or("", |service| service.name.as_str()),
                process
                    .cgroup
                    .as_ref()
                    .map_or("", |cgroup| cgroup.path.as_str()),
                pid.as_str(),
            ];
            if !haystacks.iter().any(|value| mode.matches(value, &search)) {
                return false;
            }
        }

        if let Some(user) = normalized(&self.user)
            && !mode.matches(&process.user.display_name(), &user)
        {
            return false;
        }
        if self.state.is_some_and(|state| process.state != state) {
            return false;
        }
        if self
            .min_cpu
            .is_some_and(|minimum| process.cpu_percent < minimum)
        {
            return false;
        }
        if self
            .min_memory
            .is_some_and(|minimum| process.memory_percent < minimum)
        {
            return false;
        }
        if let Some(name) = normalized(&self.name)
            && !mode.matches(&process.name, &name)
        {
            return false;
        }
        if let Some(exact_name) = normalized(&self.exact_name)
            && process.name.to_ascii_lowercase() != exact_name
        {
            return false;
        }
        if let Some(pid) = self.pid
            && process.pid.0 != pid
        {
            return false;
        }
        if let Some(ppid) = self.ppid {
            let parent = process.parent_pid.map(|id| id.0);
            if parent != Some(ppid) {
                return false;
            }
        }
        if let Some(service) = normalized(&self.service_or_cgroup) {
            let service_name = process
                .service
                .as_ref()
                .map_or("", |item| item.name.as_str());
            let cgroup = process
                .cgroup
                .as_ref()
                .map_or("", |item| item.path.as_str());
            if !mode.matches(service_name, &service) && !mode.matches(cgroup, &service) {
                return false;
            }
        }
        if let Some(cgroup_filter) = normalized(&self.cgroup) {
            let cgroup = process
                .cgroup
                .as_ref()
                .map_or("", |item| item.path.as_str());
            if !mode.matches(cgroup, &cgroup_filter) {
                return false;
            }
        }
        true
    }

    pub fn has_selector(&self) -> bool {
        self.search.is_some()
            || self.user.is_some()
            || self.state.is_some()
            || self.min_cpu.is_some()
            || self.min_memory.is_some()
            || self.name.is_some()
            || self.exact_name.is_some()
            || self.service_or_cgroup.is_some()
            || self.cgroup.is_some()
            || self.pid.is_some()
            || self.ppid.is_some()
    }
}

fn normalized(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayProcess {
    pub index: usize,
    pub depth: usize,
    pub group_label: Option<String>,
}

pub fn select_processes(
    processes: &[Process],
    filter: &ProcessFilter,
    sort_key: SortKey,
    direction: SortDirection,
    group: GroupMode,
    limit: Option<usize>,
) -> Vec<DisplayProcess> {
    let mut indices: Vec<usize> = processes
        .iter()
        .enumerate()
        .filter_map(|(index, process)| filter.matches(process).then_some(index))
        .collect();

    if group == GroupMode::Tree {
        indices = tree_order(processes, &indices, sort_key, direction);
    } else {
        indices.sort_by(|left, right| {
            compare_processes(&processes[*left], &processes[*right], sort_key)
        });
        if direction == SortDirection::Descending {
            indices.reverse();
        }
        if group != GroupMode::None {
            indices.sort_by(|left, right| {
                group_label(&processes[*left], group)
                    .cmp(&group_label(&processes[*right], group))
                    .then_with(|| {
                        compare_processes(&processes[*left], &processes[*right], sort_key)
                    })
            });
            if direction == SortDirection::Descending {
                let mut groups: Vec<Vec<usize>> = Vec::new();
                for index in indices {
                    let label = group_label(&processes[index], group);
                    if groups.last().is_none_or(|items| {
                        items
                            .first()
                            .is_none_or(|first| group_label(&processes[*first], group) != label)
                    }) {
                        groups.push(Vec::new());
                    }
                    if let Some(items) = groups.last_mut() {
                        items.push(index);
                    }
                }
                for items in &mut groups {
                    items.reverse();
                }
                indices = groups.into_iter().flatten().collect();
            }
        }
    }

    if let Some(limit) = limit {
        indices.truncate(limit);
    }

    let mut previous_group: Option<String> = None;
    indices
        .into_iter()
        .map(|index| {
            let process = &processes[index];
            let label = match group {
                GroupMode::User | GroupMode::Service => Some(group_label(process, group)),
                _ => None,
            };
            let show_label = label
                .clone()
                .filter(|label| previous_group.as_ref() != Some(label));
            if label.is_some() {
                previous_group = label;
            }
            DisplayProcess {
                index,
                depth: if group == GroupMode::Tree {
                    tree_depth(processes, process.pid)
                } else {
                    0
                },
                group_label: show_label,
            }
        })
        .collect()
}

fn tree_order(
    processes: &[Process],
    selected: &[usize],
    sort_key: SortKey,
    direction: SortDirection,
) -> Vec<usize> {
    let selected_map: HashMap<ProcessId, usize> = selected
        .iter()
        .map(|index| (processes[*index].pid, *index))
        .collect();
    let mut children: HashMap<Option<ProcessId>, Vec<usize>> = HashMap::new();
    for index in selected {
        let parent = processes[*index]
            .parent_pid
            .filter(|pid| selected_map.contains_key(pid));
        children.entry(parent).or_default().push(*index);
    }
    for values in children.values_mut() {
        values.sort_by(|left, right| {
            compare_processes(&processes[*left], &processes[*right], sort_key)
        });
        if direction == SortDirection::Descending {
            values.reverse();
        }
    }
    let mut output = Vec::with_capacity(selected.len());
    let mut visiting = HashMap::<ProcessId, bool>::new();
    append_children(None, processes, &children, &mut visiting, &mut output);
    for index in selected {
        if !output.contains(index) {
            output.push(*index);
        }
    }
    output
}

fn append_children(
    parent: Option<ProcessId>,
    processes: &[Process],
    children: &HashMap<Option<ProcessId>, Vec<usize>>,
    visiting: &mut HashMap<ProcessId, bool>,
    output: &mut Vec<usize>,
) {
    if let Some(items) = children.get(&parent) {
        for index in items {
            let pid = processes[*index].pid;
            if visiting.insert(pid, true).is_some() {
                continue;
            }
            output.push(*index);
            append_children(Some(pid), processes, children, visiting, output);
            visiting.remove(&pid);
        }
    }
}

fn tree_depth(processes: &[Process], pid: ProcessId) -> usize {
    let by_pid: HashMap<ProcessId, &Process> = processes
        .iter()
        .map(|process| (process.pid, process))
        .collect();
    let mut depth = 0usize;
    let mut current = pid;
    let mut seen = HashMap::new();
    while let Some(parent) = by_pid.get(&current).and_then(|process| process.parent_pid) {
        if seen.insert(parent, true).is_some() || depth >= 32 {
            break;
        }
        depth += 1;
        current = parent;
    }
    depth
}

fn group_label(process: &Process, group: GroupMode) -> String {
    match group {
        GroupMode::User => process.user.display_name(),
        GroupMode::Service => process
            .service
            .as_ref()
            .map(|service| service.name.clone())
            .or_else(|| process.cgroup.as_ref().map(|cgroup| cgroup.path.clone()))
            .unwrap_or_else(|| "ungrouped".to_owned()),
        GroupMode::None | GroupMode::Tree => String::new(),
    }
}

fn compare_processes(left: &Process, right: &Process, key: SortKey) -> Ordering {
    let primary = match key {
        SortKey::Cpu => left.cpu_percent.total_cmp(&right.cpu_percent),
        SortKey::Memory => left.memory_percent.total_cmp(&right.memory_percent),
        SortKey::Pid => left.pid.cmp(&right.pid),
        SortKey::Name => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
        SortKey::User => left.user.display_name().cmp(&right.user.display_name()),
        SortKey::Runtime => left.runtime_seconds.cmp(&right.runtime_seconds),
        SortKey::ReadRate => left
            .io
            .read_bytes_per_second
            .total_cmp(&right.io.read_bytes_per_second),
        SortKey::WriteRate => left
            .io
            .write_bytes_per_second
            .total_cmp(&right.io.write_bytes_per_second),
        SortKey::Threads => left.threads.cmp(&right.threads),
    };
    primary
        .then_with(|| left.memory_percent.total_cmp(&right.memory_percent))
        .then_with(|| left.pid.cmp(&right.pid))
}

pub fn parse_filter_expression(expression: &str) -> ProcessFilter {
    let mut filter = ProcessFilter::default();
    let mut free = Vec::new();
    for token in expression.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "user" => filter.user = Some(value.to_owned()),
                "state" => filter.state = parse_state(value),
                "cpu" => filter.min_cpu = parse_threshold(value),
                "mem" | "memory" => filter.min_memory = parse_threshold(value),
                "name" => filter.name = Some(value.to_owned()),
                "service" | "cgroup" => filter.service_or_cgroup = Some(value.to_owned()),
                _ => free.push(token),
            }
        } else {
            free.push(token);
        }
    }
    if !free.is_empty() {
        filter.search = Some(free.join(" "));
    }
    filter
}

fn parse_threshold(value: &str) -> Option<f64> {
    value.trim_start_matches(['>', '=']).parse().ok()
}

pub fn parse_state(value: &str) -> Option<ProcessState> {
    match value.to_ascii_lowercase().as_str() {
        "r" | "running" => Some(ProcessState::Running),
        "s" | "sleeping" => Some(ProcessState::Sleeping),
        "d" | "disk" | "disk-sleep" => Some(ProcessState::DiskSleep),
        "t" | "stopped" => Some(ProcessState::Stopped),
        "z" | "zombie" => Some(ProcessState::Zombie),
        "i" | "idle" => Some(ProcessState::Idle),
        "x" | "dead" => Some(ProcessState::Dead),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use lens_model::{IoCounters, Process, User};

    use super::*;

    fn process(pid: u32, name: &str, cpu: f64) -> Process {
        Process {
            pid: ProcessId(pid),
            parent_pid: None,
            name: name.to_owned(),
            command_line: Some(format!("{name} --worker")),
            executable: None,
            user: User {
                uid: 1000,
                name: Some("operator".to_owned()),
            },
            state: ProcessState::Sleeping,
            cpu_percent: cpu,
            memory_percent: 1.0,
            rss_bytes: 1,
            virtual_memory_bytes: 2,
            threads: 1,
            io: IoCounters::default(),
            runtime_seconds: 1,
            cgroup: None,
            service: None,
            container: None,
            file_descriptor_count: None,
            child_pids: Vec::new(),
            unavailable_fields: Vec::new(),
            cpu_time_ticks: 0,
            start_time_ticks: u64::from(pid),
        }
    }

    #[test]
    fn defaults_to_cpu_descending() {
        let items = vec![process(1, "low", 1.0), process(2, "high", 9.0)];
        let selected = select_processes(
            &items,
            &ProcessFilter::default(),
            SortKey::Cpu,
            SortDirection::Descending,
            GroupMode::None,
            None,
        );
        assert_eq!(items[selected[0].index].pid, ProcessId(2));
    }

    #[test]
    fn parses_shared_filter_grammar() {
        let filter = parse_filter_expression("user:postgres state:zombie cpu:>5 nginx");
        assert_eq!(filter.user.as_deref(), Some("postgres"));
        assert_eq!(filter.state, Some(ProcessState::Zombie));
        assert_eq!(filter.min_cpu, Some(5.0));
        assert_eq!(filter.search.as_deref(), Some("nginx"));
    }
}
