#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_view(lens_system::View::Services) {
        if lens_system::is_broken_pipe(&error) {
            return;
        }
        eprintln!("lens-services: {error:#}");
        std::process::exit(1);
    }
}
