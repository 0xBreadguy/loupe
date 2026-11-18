//! Windows: the same names as `unix.rs`, as stubs. Nothing here draws. Starting the presenter
//! fails at the first call with a plain "not supported yet" message, which `main` prints.
//!
//! This file uses only `std`. Test builds on other hosts compile it so its signatures and its
//! message stay checked even though nothing can cross-compile for Windows from here. The real
//! backend (console modes, a reader thread, a resize poll) is a later step.
use super::{Ready, Signals, not_supported};
use std::io;
use std::time::Duration;

pub struct Wake(());

#[derive(Clone, Copy, Debug, Default)]
pub struct Waker(());

impl Wake {
    pub fn new() -> io::Result<Self> {
        Err(not_supported("the terminal presenter"))
    }
    pub fn waker(&self) -> Waker {
        Waker(())
    }
    pub fn drain(&self) {}
}

impl Waker {
    pub fn wake(self) {}
}

pub fn wait(_wake: &Wake, _timeout: Option<Duration>) -> io::Result<Ready> {
    Err(not_supported("waiting on the console"))
}

pub fn write_all_fd(_fd: i32, _data: &[u8]) -> io::Result<()> {
    Err(not_supported("writing to a descriptor"))
}

pub fn write_stdout(_data: &[u8]) -> io::Result<()> {
    Err(not_supported("writing to the console"))
}

pub fn install_signals(_waker: Waker) -> io::Result<()> {
    Err(not_supported("signal handling"))
}

pub fn take_signals() -> Signals {
    Signals::default()
}

pub struct Restorer(());

pub fn enter_raw() -> io::Result<Restorer> {
    Err(not_supported("raw console mode"))
}

pub fn restore_grapheme_mode_on_exit() {}

pub fn restore_theme_mode_on_exit() {}

pub fn window_size() -> io::Result<(usize, usize)> {
    Err(not_supported("reading the console size"))
}

pub fn read_input() -> io::Result<(Vec<u8>, bool)> {
    Err(not_supported("reading console input"))
}
