use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use crossterm::event::{KeyCode, KeyEvent};
use lens_core::{
    DisplayProcess, GroupMode, ProcessFilter, SortDirection, SortKey, parse_filter_expression,
    select_processes,
};
use lens_model::{Process, ProcessId, Snapshot};

use crate::{TerminalCapabilities, UiOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    Refresh,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Search,
    Filter,
}

#[derive(Debug)]
pub struct App {
    pub snapshot: Snapshot,
    pub selected: usize,
    pub view: View,
    inspected: Option<(ProcessId, u64)>,
    pub search: String,
    pub filter_expression: String,
    pub input_mode: Option<InputMode>,
    pub input_buffer: String,
    pub show_help: bool,
    pub show_sort: bool,
    pub sort_selection: usize,
    pub paused: bool,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    pub group: GroupMode,
    pub base_filter: ProcessFilter,
    pub limit: Option<usize>,
    pub error: Option<String>,
    pub capabilities: TerminalCapabilities,
    interval: Duration,
    history_length: usize,
    host_cpu_history: VecDeque<u64>,
    memory_history: VecDeque<u64>,
    process_cpu_history: HashMap<(ProcessId, u64), VecDeque<u64>>,
    process_memory_history: HashMap<(ProcessId, u64), VecDeque<u64>>,
}

impl App {
    pub fn new(snapshot: Snapshot, options: UiOptions, capabilities: TerminalCapabilities) -> Self {
        let mut app = Self {
            snapshot,
            selected: 0,
            view: View::List,
            inspected: None,
            search: String::new(),
            filter_expression: String::new(),
            input_mode: None,
            input_buffer: String::new(),
            show_help: false,
            show_sort: false,
            sort_selection: SortKey::ALL
                .iter()
                .position(|key| *key == options.sort_key)
                .unwrap_or_default(),
            paused: false,
            sort_key: options.sort_key,
            sort_direction: options.sort_direction,
            group: options.group,
            base_filter: options.filter,
            limit: options.limit,
            error: None,
            capabilities,
            interval: options.interval,
            history_length: options.history_length.max(2),
            host_cpu_history: VecDeque::new(),
            memory_history: VecDeque::new(),
            process_cpu_history: HashMap::new(),
            process_memory_history: HashMap::new(),
        };
        app.record_history();
        app
    }

    pub const fn interval(&self) -> Duration {
        self.interval
    }

    pub const fn paused(&self) -> bool {
        self.paused
    }

    pub fn replace_snapshot(&mut self, snapshot: Snapshot) {
        let selected_identity = self.selected_process().map(Process::identity);
        self.snapshot = snapshot;
        self.record_history();
        self.restore_selection(selected_identity);
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn visible(&self) -> Vec<DisplayProcess> {
        let mut filter = self.base_filter.clone();
        if !self.search.is_empty() {
            filter.search = Some(self.search.clone());
        }
        if !self.filter_expression.is_empty() {
            merge_filter(
                &mut filter,
                parse_filter_expression(&self.filter_expression),
            );
        }
        select_processes(
            &self.snapshot.processes,
            &filter,
            self.sort_key,
            self.sort_direction,
            self.group,
            self.limit,
        )
    }

    pub fn selected_process(&self) -> Option<&Process> {
        if self.view == View::Detail {
            let identity = self.inspected?;
            return self
                .snapshot
                .processes
                .iter()
                .find(|process| process.identity() == identity);
        }
        self.process_at_selected_index()
    }

    pub const fn inspected_process_identity(&self) -> Option<(ProcessId, u64)> {
        self.inspected
    }

    fn process_at_selected_index(&self) -> Option<&Process> {
        let visible = self.visible();
        visible
            .get(self.selected)
            .and_then(|item| self.snapshot.processes.get(item.index))
    }

    pub fn host_cpu_history(&self) -> Vec<u64> {
        self.host_cpu_history.iter().copied().collect()
    }

    pub fn memory_history(&self) -> Vec<u64> {
        self.memory_history.iter().copied().collect()
    }

    pub fn selected_cpu_history(&self) -> Vec<u64> {
        self.selected_process()
            .and_then(|process| {
                self.process_cpu_history
                    .get(&(process.pid, process.start_time_ticks))
            })
            .map_or_else(Vec::new, |values| values.iter().copied().collect())
    }

    pub fn selected_memory_history(&self) -> Vec<u64> {
        self.selected_process()
            .and_then(|process| {
                self.process_memory_history
                    .get(&(process.pid, process.start_time_ticks))
            })
            .map_or_else(Vec::new, |values| values.iter().copied().collect())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if self.input_mode.is_some() {
            return self.handle_input_key(key);
        }
        if self.show_help {
            self.show_help = false;
            return Action::Redraw;
        }
        if self.show_sort {
            return self.handle_sort_key(key);
        }

        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc => {
                if self.view == View::Detail {
                    self.view = View::List;
                    self.inspected = None;
                    Action::Redraw
                } else {
                    Action::Quit
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Action::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Action::Redraw
            }
            KeyCode::Enter => {
                if let Some(identity) = self.process_at_selected_index().map(Process::identity) {
                    self.inspected = Some(identity);
                    self.view = View::Detail;
                    Action::Redraw
                } else {
                    Action::None
                }
            }
            KeyCode::Char('/') => {
                self.input_mode = Some(InputMode::Search);
                self.input_buffer.clone_from(&self.search);
                Action::Redraw
            }
            KeyCode::Char('f') => {
                self.input_mode = Some(InputMode::Filter);
                self.input_buffer.clone_from(&self.filter_expression);
                Action::Redraw
            }
            KeyCode::Char('s') => {
                self.show_sort = true;
                self.sort_selection = SortKey::ALL
                    .iter()
                    .position(|key| *key == self.sort_key)
                    .unwrap_or_default();
                Action::Redraw
            }
            KeyCode::Char('g') => {
                self.group = self.group.next();
                self.selected = 0;
                Action::Redraw
            }
            KeyCode::Char(' ') => {
                self.paused = !self.paused;
                Action::Redraw
            }
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('?') => {
                self.show_help = true;
                Action::Redraw
            }
            KeyCode::Tab => {
                self.move_selection(1);
                Action::Redraw
            }
            KeyCode::BackTab => {
                self.move_selection(-1);
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = None;
                self.input_buffer.clear();
            }
            KeyCode::Enter => {
                match self.input_mode {
                    Some(InputMode::Search) => self.search.clone_from(&self.input_buffer),
                    Some(InputMode::Filter) => {
                        self.filter_expression.clone_from(&self.input_buffer);
                    }
                    None => {}
                }
                self.input_mode = None;
                self.selected = 0;
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(character) => self.input_buffer.push(character),
            _ => return Action::None,
        }
        Action::Redraw
    }

    fn handle_sort_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => self.show_sort = false,
            KeyCode::Down | KeyCode::Char('j') => {
                self.sort_selection = (self.sort_selection + 1) % SortKey::ALL.len();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sort_selection = self
                    .sort_selection
                    .checked_sub(1)
                    .unwrap_or(SortKey::ALL.len() - 1);
            }
            KeyCode::Char('d') => {
                self.sort_direction = match self.sort_direction {
                    SortDirection::Ascending => SortDirection::Descending,
                    SortDirection::Descending => SortDirection::Ascending,
                };
            }
            KeyCode::Enter => {
                self.sort_key = SortKey::ALL[self.sort_selection];
                self.show_sort = false;
                self.selected = 0;
            }
            _ => return Action::None,
        }
        Action::Redraw
    }

    fn move_selection(&mut self, delta: isize) {
        let length = self.visible().len();
        if length == 0 {
            self.selected = 0;
            return;
        }
        if delta > 0 {
            self.selected = (self.selected + 1).min(length - 1);
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
        if self.view == View::Detail {
            self.inspected = self.process_at_selected_index().map(Process::identity);
        }
    }

    fn restore_selection(&mut self, identity: Option<(ProcessId, u64)>) {
        if let Some(identity) = identity
            && let Some(position) = self
                .visible()
                .iter()
                .position(|item| self.snapshot.processes[item.index].identity() == identity)
        {
            self.selected = position;
            return;
        }

        let length = self.visible().len();
        if length == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(length - 1);
        }
    }

    fn record_history(&mut self) {
        push_bounded(
            &mut self.host_cpu_history,
            self.snapshot.host.cpu_percent.clamp(0.0, 100.0) as u64,
            self.history_length,
        );
        push_bounded(
            &mut self.memory_history,
            self.snapshot.host.memory.used_percent().clamp(0.0, 100.0) as u64,
            self.history_length,
        );
        for process in &self.snapshot.processes {
            let key = (process.pid, process.start_time_ticks);
            push_bounded(
                self.process_cpu_history.entry(key).or_default(),
                process.cpu_percent.max(0.0) as u64,
                self.history_length,
            );
            push_bounded(
                self.process_memory_history.entry(key).or_default(),
                process.rss_bytes / 1024,
                self.history_length,
            );
        }
        self.process_cpu_history.retain(|key, _| {
            self.snapshot
                .processes
                .iter()
                .any(|p| (p.pid, p.start_time_ticks) == *key)
        });
        self.process_memory_history.retain(|key, _| {
            self.snapshot
                .processes
                .iter()
                .any(|p| (p.pid, p.start_time_ticks) == *key)
        });
    }
}

fn push_bounded(values: &mut VecDeque<u64>, value: u64, capacity: usize) {
    values.push_back(value);
    while values.len() > capacity {
        values.pop_front();
    }
}

fn merge_filter(target: &mut ProcessFilter, extra: ProcessFilter) {
    if extra.search.is_some() {
        target.search = extra.search;
    }
    if extra.user.is_some() {
        target.user = extra.user;
    }
    if extra.state.is_some() {
        target.state = extra.state;
    }
    if extra.min_cpu.is_some() {
        target.min_cpu = extra.min_cpu;
    }
    if extra.min_memory.is_some() {
        target.min_memory = extra.min_memory;
    }
    if extra.name.is_some() {
        target.name = extra.name;
    }
    if extra.service_or_cgroup.is_some() {
        target.service_or_cgroup = extra.service_or_cgroup;
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use lens_core::{GroupMode, ProcessFilter, SortDirection, SortKey};
    use lens_model::{IoCounters, ProcessState, User};

    use super::*;
    use crate::ColorMode;

    fn options() -> UiOptions {
        UiOptions {
            interval: Duration::from_secs(1),
            sort_key: SortKey::Cpu,
            sort_direction: SortDirection::Descending,
            group: GroupMode::None,
            filter: ProcessFilter::default(),
            limit: None,
            history_length: 10,
            color_mode: ColorMode::Never,
            ascii: true,
        }
    }

    fn process(pid: u32, start_time_ticks: u64, name: &str, cpu_percent: f64) -> Process {
        Process {
            pid: ProcessId(pid),
            parent_pid: None,
            name: name.to_owned(),
            command_line: Some(name.to_owned()),
            executable: None,
            user: User {
                uid: 501,
                name: Some("operator".to_owned()),
            },
            state: ProcessState::Sleeping,
            cpu_percent,
            memory_percent: 1.0,
            rss_bytes: 1024,
            virtual_memory_bytes: 2048,
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
            start_time_ticks,
        }
    }

    fn snapshot(processes: Vec<Process>) -> Snapshot {
        let mut snapshot = Snapshot::empty("fixture");
        snapshot.processes = processes;
        snapshot
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn inspected_process_stays_pinned_when_cpu_sort_order_changes() {
        let mut app = App::new(
            snapshot(vec![
                process(10, 100, "busy", 80.0),
                process(20, 200, "selected", 20.0),
            ]),
            options(),
            TerminalCapabilities::detect(ColorMode::Never, true),
        );
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        app.replace_snapshot(snapshot(vec![
            process(10, 100, "busy", 1.0),
            process(20, 200, "selected", 95.0),
        ]));

        assert_eq!(
            app.selected_process().map(Process::identity),
            Some((ProcessId(20), 200))
        );
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn inspected_process_does_not_follow_a_reused_pid() {
        let mut app = App::new(
            snapshot(vec![process(20, 200, "selected", 20.0)]),
            options(),
            TerminalCapabilities::detect(ColorMode::Never, true),
        );
        app.handle_key(key(KeyCode::Enter));

        app.replace_snapshot(snapshot(vec![process(20, 999, "replacement", 20.0)]));

        assert!(app.selected_process().is_none());
        assert_eq!(app.inspected_process_identity(), Some((ProcessId(20), 200)));
    }

    #[test]
    fn list_selection_stays_on_the_same_process_when_sort_order_changes() {
        let mut app = App::new(
            snapshot(vec![
                process(10, 100, "busy", 80.0),
                process(20, 200, "selected", 20.0),
            ]),
            options(),
            TerminalCapabilities::detect(ColorMode::Never, true),
        );
        app.handle_key(key(KeyCode::Down));

        app.replace_snapshot(snapshot(vec![
            process(10, 100, "busy", 1.0),
            process(20, 200, "selected", 95.0),
        ]));

        assert_eq!(
            app.selected_process().map(Process::identity),
            Some((ProcessId(20), 200))
        );
        assert_eq!(app.selected, 0);
    }
}
