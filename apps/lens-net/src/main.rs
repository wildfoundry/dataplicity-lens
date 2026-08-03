#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_view(lens_system::View::Net) {
        if lens_system::is_broken_pipe(&error) {
            return;
        }
        eprintln!("lens-net: {error:#}");
        std::process::exit(1);
    }
}
