#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_view(lens_system::View::Containers) {
        if lens_system::is_broken_pipe(&error) {
            return;
        }
        eprintln!("lens-containers: {error:#}");
        std::process::exit(lens_system::exit_code(&error));
    }
}
