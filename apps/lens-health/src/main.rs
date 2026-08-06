#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_view(lens_system::View::Health) {
        if lens_system::is_broken_pipe(&error) {
            return;
        }
        eprintln!("lens-health: {error:#}");
        std::process::exit(1);
    }
}
