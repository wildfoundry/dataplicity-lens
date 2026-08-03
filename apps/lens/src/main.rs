#![forbid(unsafe_code)]

fn main() {
    if let Err(error) = lens_system::run_cockpit() {
        eprintln!("lens: {error:#}");
        std::process::exit(1);
    }
}
