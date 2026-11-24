//! Terminal presenter for the Loupe cell grid.
//!
//! The core crate composes the screen. This crate diffs that grid against the
//! previous frame and writes colors at the depth the terminal reports.
mod caps;
mod color;
pub use color::{BACKGROUND_QUERY, THEME_MODE_OFF, startup_probe, startup_probe_for};
mod encode;
mod keys;
pub mod insights_data;
pub mod platform;
pub mod storage;

pub use encode::{ENTER_TERM, Encoder, LEAVE_TERM, Screen, ScreenCell, screen_matches};
pub use caps::{Change, GRAPHEME_OFF, GRAPHEME_ON, PROBE, Probe, WINDOW, wanted as probe_wanted};
pub use keys::{Event, Parser, Report};
pub use platform::write_all_fd;
use loupe_core::{model::State, terminal::Canvas, ui};

/// Draw one frame the way the dashboard does: the grid is the whole terminal,
/// with no padding, and a system-reduced copy is what gets composed when the
/// presenter has asked for reduced motion.
pub fn compose(s: &mut State, cols: usize, rows: usize) -> Canvas {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let reduced = s.surface.borrow().system_reduced;
    let palette = s.palette;
    let (canvas, mut surface) = if reduced && !s.calm {
        let mut ghost = s.clone();
        ghost.calm = true;
        ghost.fx = false;
        ui::draw_grid(&ghost, cols, rows, &palette)
    } else {
        ui::draw_grid(s, cols, rows, &palette)
    };
    surface.system_reduced = reduced;
    *s.surface.borrow_mut() = surface;
    canvas
}

/// Whether the virtual screen shows the canvas as the original would draw it: laid out by
/// [`loupe_core::cluster::lay_out`] with the screen's width method, then cell by cell.
pub fn cells_match(screen: &Screen, canvas: &Canvas) -> Result<(), String> {
    screen_matches(screen, &loupe_core::cluster::lay_out(canvas, screen.method))
}

#[cfg(all(test, unix))]
mod write_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    #[test]
    fn write_all_survives_a_full_pipe() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let flags = unsafe { libc::fcntl(fds[1], libc::F_GETFL) };
        unsafe { libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK) };
        let blob = vec![b'x'; 256 * 1024];
        let writer = std::thread::spawn(move || {
            let result = write_all_fd(fds[1], &blob);
            unsafe { libc::close(fds[1]) };
            result.unwrap();
        });
        let mut reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut got = Vec::new();
        reader.read_to_end(&mut got).unwrap();
        writer.join().unwrap();
        assert_eq!(got.len(), 256 * 1024);
        assert!(got.iter().all(|b| *b == b'x'));
    }
}
