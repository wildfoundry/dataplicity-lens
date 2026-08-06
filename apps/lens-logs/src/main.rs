#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_view(lens_system::View::Logs) {
        if lens_system::is_broken_pipe(&error) {
            return;
        }
        eprintln!("lens-logs: {error:#}");
        std::process::exit(1);
    }
}
