//! The platform boundary for the presenter: everything that touches the terminal driver, the
//! wait, signals and the wake channel. `main.rs` and the rest of the crate call only the names
//! this module exports and never `libc` directly.
//!
//! `unix.rs` is Linux and macOS, one code path for both: a pipe for waking the loop, `pselect`
//! to wait on the terminal and the pipe, termios for raw mode. `windows.rs` has the same names
//! with stubs that say "not supported yet"; it uses only `std`, so test builds on every host
//! compile it and the stubs cannot rot unseen. The Windows console backend is a later step.
#[cfg(any(windows, test))]
use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What every Windows stub says.
pub const NOT_SUPPORTED_YET: &str = "not supported yet on Windows";

#[cfg(any(windows, test))]
pub(crate) fn not_supported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("{what}: {NOT_SUPPORTED_YET}"))
}

/// What woke the loop: bytes waiting on the terminal, the wake channel, or neither (a timeout,
/// or a signal that interrupted the wait).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ready {
    pub input: bool,
    pub wake: bool,
}

/// Signals the handlers recorded since the last call to `take_signals`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    /// The window changed size.
    pub resized: bool,
    /// Interrupt, terminate or hangup: leave the way a quit key does.
    pub quit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// The Windows file compiles on every host and refuses plainly. Nothing real runs on Windows.
    #[test]
    fn windows_stubs_refuse_plainly() {
        let err = windows::Wake::new().err().expect("Wake::new must refuse");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("not supported yet"), "{err}");
        for err in [
            windows::enter_raw().err().unwrap(),
            windows::window_size().unwrap_err(),
            windows::read_input().unwrap_err(),
            windows::write_stdout(b"x").unwrap_err(),
            windows::install_signals(windows::Waker::default()).unwrap_err(),
        ] {
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
            assert!(err.to_string().contains("not supported yet"), "{err}");
        }
        assert_eq!(windows::take_signals(), Signals::default());
    }
}
