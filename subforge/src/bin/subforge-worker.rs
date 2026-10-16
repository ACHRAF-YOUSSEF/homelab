#![forbid(unsafe_code)]

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Subforge worker {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Err(error) = subforge::remote::run_worker() {
        eprintln!("Subforge worker startup failed: {error}");
        std::process::exit(2);
    }
}
