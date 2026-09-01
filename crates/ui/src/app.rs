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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Redraw,
    Refresh,
    RunDiagnostic(String),
    ExecuteProcessSignal {
        signal: ProcessSignal,
        pid: u32,
        start_time_ticks: u64,
    },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSignal {
    Term,
    Hup,
    Int,
    Stop,
    Cont,
    Kill,
}

impl ProcessSignal {
    pub const ALL: [Self; 6] = [
        Self::Term,
        Self::Hup,
        Self::Int,
        Self::Stop,
        Self::Cont,
        Self::Kill,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Term => "TERM · ask the process to exit",
            Self::Hup => "HUP · reload or reopen files",
            Self::Int => "INT · interrupt the process",
            Self::Stop => "STOP · suspend the process",
            Self::Cont => "CONT · resume the process",
            Self::Kill => "KILL · force immediate exit",
        }
    }

    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Term => "term",
            Self::Hup => "hup",
            Self::Int => "int",
            Self::Stop => "stop",
            Self::Cont => "cont",
            Self::Kill => "kill",
        }
    }

    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Hup => "HUP",
            Self::Int => "INT",
            Self::Stop => "STOP",
            Self::Cont => "CONT",
            Self::Kill => "KILL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessActionStage {
    Choose,
    Confirm,
    Running,
    Result,
}

#[derive(Debug)]
pub struct ProcessActionDialog {
    pub pid: u32,
    pub start_time_ticks: u64,
    pub process: String,
    pub selection: usize,
    pub stage: ProcessActionStage,
    pub result: String,
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
    pub diagnostic_open: bool,
    pub diagnostic_input: String,
    pub diagnostic_output: Vec<String>,
    pub diagnostic_running: bool,
    pub process_action: Option<ProcessActionDialog>,
    pub sort_selection: usize,
    pub paused: bool,
    pub collecting: bool,
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
            diagnostic_open: false,
            diagnostic_input: String::new(),
            diagnostic_output: Vec::new(),
            diagnostic_running: false,
            process_action: None,
            sort_selection: SortKey::ALL
                .iter()
                .position(|key| *key == options.sort_key)
                .unwrap_or_default(),
            paused: false,
            collecting: false,
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

    /// How many samples the charts can ever hold, whether or not they have filled yet.
    pub const fn history_capacity(&self) -> usize {
        self.history_length
    }

    pub const fn paused(&self) -> bool {
        self.paused
    }

    pub const fn collecting(&self) -> bool {
        self.collecting
    }

    pub const fn set_collecting(&mut self, collecting: bool) {
        self.collecting = collecting;
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
        if self.process_action.is_some() {
            return self.handle_process_action_key(key);
        }
        if self.diagnostic_open {
            return self.handle_diagnostic_key(key);
        }
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
            KeyCode::Char('a') => {
                if let Some(process) = self.selected_process() {
                    self.process_action = Some(ProcessActionDialog {
                        pid: process.pid.0,
                        start_time_ticks: process.start_time_ticks,
                        process: process.name.clone(),
                        selection: 0,
                        stage: ProcessActionStage::Choose,
                        result: String::new(),
                    });
                    Action::Redraw
                } else {
                    Action::None
                }
            }
            KeyCode::Char('!') => {
                self.diagnostic_open = true;
                Action::Redraw
            }
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

    fn handle_process_action_key(&mut self, key: KeyEvent) -> Action {
        let dialog = self.process_action.as_mut().expect("action dialog");
        match dialog.stage {
            ProcessActionStage::Choose => match key.code {
                KeyCode::Esc => self.process_action = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    dialog.selection = (dialog.selection + 1).min(ProcessSignal::ALL.len() - 1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    dialog.selection = dialog.selection.saturating_sub(1);
                }
                KeyCode::Enter => dialog.stage = ProcessActionStage::Confirm,
                _ => return Action::None,
            },
            ProcessActionStage::Confirm => match key.code {
                KeyCode::Esc => dialog.stage = ProcessActionStage::Choose,
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    dialog.stage = ProcessActionStage::Running;
                    return Action::ExecuteProcessSignal {
                        signal: ProcessSignal::ALL[dialog.selection],
                        pid: dialog.pid,
                        start_time_ticks: dialog.start_time_ticks,
                    };
                }
                _ => return Action::None,
            },
            ProcessActionStage::Running => return Action::None,
            ProcessActionStage::Result => match key.code {
                KeyCode::Esc | KeyCode::Enter => self.process_action = None,
                _ => return Action::None,
            },
        }
        Action::Redraw
    }

    pub fn finish_process_action(&mut self, result: String) {
        if let Some(dialog) = self.process_action.as_mut() {
            dialog.result = result;
            dialog.stage = ProcessActionStage::Result;
        }
    }

    fn handle_diagnostic_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.diagnostic_open = false;
                Action::Redraw
            }
            KeyCode::Enter
                if !self.diagnostic_running && !self.diagnostic_input.trim().is_empty() =>
            {
                let command = self.diagnostic_input.trim().to_owned();
                self.diagnostic_output.push(format!("$ {command}"));
                self.diagnostic_input.clear();
                self.diagnostic_running = true;
                Action::RunDiagnostic(command)
            }
            KeyCode::Backspace if !self.diagnostic_running => {
                self.diagnostic_input.pop();
                Action::Redraw
            }
            KeyCode::Char(character) if !self.diagnostic_running => {
                self.diagnostic_input.push(character);
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    pub fn finish_diagnostic(&mut self, output: String) {
        self.diagnostic_output
            .extend(output.lines().map(str::to_owned));
        if self.diagnostic_output.len() > 500 {
            let excess = self.diagnostic_output.len() - 500;
            self.diagnostic_output.drain(..excess);
        }
        self.diagnostic_running = false;
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
    use lens_core::{GlyphMode, GroupMode, ProcessFilter, SortDirection, SortKey};
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
            theme_mode: crate::ThemeMode::Auto,
            glyphs: GlyphMode::Ascii,
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
            TerminalCapabilities::detect(ColorMode::Never, GlyphMode::Ascii),
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
            TerminalCapabilities::detect(ColorMode::Never, GlyphMode::Ascii),
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
            TerminalCapabilities::detect(ColorMode::Never, GlyphMode::Ascii),
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

    #[test]
    fn diagnostic_shell_is_explicit_and_returns_the_typed_command() {
        let mut app = App::new(
            snapshot(Vec::new()),
            options(),
            TerminalCapabilities::detect(ColorMode::Never, GlyphMode::Ascii),
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('!'))), Action::Redraw);
        for character in "printf lens".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::RunDiagnostic("printf lens".to_owned())
        );
        assert!(app.diagnostic_running);
        app.finish_diagnostic("lens".to_owned());
        assert!(!app.diagnostic_running);
        assert_eq!(
            app.diagnostic_output.last().map(String::as_str),
            Some("lens")
        );
    }

    #[test]
    fn process_action_pins_the_target_and_requires_confirmation() {
        let mut app = App::new(
            snapshot(vec![process(42, 9001, "worker", 20.0)]),
            options(),
            TerminalCapabilities::detect(ColorMode::Never, GlyphMode::Ascii),
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('a'))), Action::Redraw);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Redraw);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            Action::ExecuteProcessSignal {
                signal: ProcessSignal::Term,
                pid: 42,
                start_time_ticks: 9001,
            }
        );
        assert_eq!(
            app.process_action.as_ref().map(|dialog| dialog.stage),
            Some(ProcessActionStage::Running)
        );
        app.finish_process_action("TERM PID 42: completed".into());
        assert_eq!(
            app.process_action.as_ref().map(|dialog| dialog.stage),
            Some(ProcessActionStage::Result)
        );
    }
}
