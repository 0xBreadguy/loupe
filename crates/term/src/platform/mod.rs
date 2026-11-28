//! The platform boundary for the presenter: everything that touches the terminal driver, the
//! wait, signals and the wake channel. `main.rs` and the rest of the crate call only the names
//! this module exports and never `libc` directly.
//!
//! `unix.rs` is Linux and macOS, one code path for both: a pipe for waking the loop, `pselect`
//! to wait on the terminal and the pipe, termios for raw mode. `windows.rs` has the same names
//! with stubs that say "not supported yet"; it uses only `std`, so test builds on every host
//! compile it and the stubs cannot rot unseen. The Windows console backend is a later step.
#[cfg(all(not(windows), test))]
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

#[cfg(all(not(windows), test))]
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

    /// The non-Windows fallback stubs compile on Unix hosts and refuse plainly.
    #[cfg(not(windows))]
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

    #[cfg(windows)]
    #[test]
    fn windows_wake_and_utf16_decoding_work() {
        use std::time::{Duration, Instant};

        let wake = Wake::new().expect("Wake::new succeeds on Windows");
        let started = Instant::now();
        let ready = wait(&wake, Some(Duration::from_millis(80))).unwrap();
        assert_eq!(ready, Ready::default());
        assert!(started.elapsed() >= Duration::from_millis(60));

        let waker = wake.waker();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            waker.wake();
        });
        let ready = wait(&wake, None).unwrap();
        handle.join().unwrap();
        assert_eq!(ready, Ready { input: false, wake: true });
        assert!(wait(&wake, Some(Duration::ZERO)).unwrap().wake);
        wake.drain();
        assert!(!wait(&wake, Some(Duration::ZERO)).unwrap().wake);

        for _ in 0..10_000 {
            wake.waker().wake();
        }
        assert!(wait(&wake, Some(Duration::ZERO)).unwrap().wake);
        wake.drain();
        assert!(!wait(&wake, Some(Duration::ZERO)).unwrap().wake);

        // Surrogate pair split across two reads decodes to the complete UTF-8 scalar.
        let pair: Vec<u16> = "a🍞\x1b[A".encode_utf16().collect();
        let mut bytes = Vec::new();
        let mut hi = None;
        windows::push_utf16_for_test(&mut bytes, &mut hi, &pair[..2]);
        windows::push_utf16_for_test(&mut bytes, &mut hi, &pair[2..]);
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "a🍞\x1b[A");

        assert_eq!(
            write_all_fd(99, b"x").unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );

        // UTF-8 safe chunking never cuts inside a 3-byte or 4-byte scalar.
        let text = "ab─🍞z".as_bytes(); // 2 + 3 + 4 + 1 = 10 bytes
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 3), 2);
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 4), 2);
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 5), 5);
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 6), 5);
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 8), 5);
        assert_eq!(windows::utf8_safe_chunk_for_test(text, 9), 9);
    }
}
