//! Service backend over the App engine (port of nplay/service/backend.py). Owner: backend track.

/// `nplay --serve`: run the control service in the foreground; returns the exit code.
pub fn run_service(_path: Option<std::path::PathBuf>, _idle_exit: u64) -> i32 {
    eprintln!("NPLAY service: not implemented yet");
    1
}
