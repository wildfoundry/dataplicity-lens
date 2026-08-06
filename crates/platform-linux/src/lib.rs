#![forbid(unsafe_code)]

mod collector;
mod proc_parse;

pub use collector::{CollectError, LinuxCollector};
pub use proc_parse::{parse_meminfo, parse_pid_stat, parse_proc_stat};
