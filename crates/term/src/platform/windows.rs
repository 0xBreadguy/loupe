//! Windows terminal platform backend: Win32 console modes, VT input/output,
//! event-based wake channel, console input reader thread, resize watcher, and Ctrl handler.
//!
//! When compiled on non-Windows hosts (`cargo test` cross-compile checks), keeps `std`-only stubs.
use super::{Ready, Signals};
#[cfg(not(windows))]
use super::not_supported;
#[cfg(not(windows))]
use std::io;
#[cfg(not(windows))]
use std::time::Duration;

#[cfg(windows)]
mod tray;
#[cfg(windows)]
pub use tray::run_tray;

#[cfg(windows)]
mod win32 {
    use super::{Ready, Signals};
    use crate::{ENTER_TERM, GRAPHEME_OFF, LEAVE_TERM, THEME_MODE_OFF};
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::io::{self, ErrorKind};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
    use std::time::Duration;

    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const STD_ERROR_HANDLE: u32 = (-12i32) as u32;
    const INVALID_HANDLE_VALUE: isize = -1;

    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_MOUSE_INPUT: u32 = 0x0010;
    const ENABLE_QUICK_EDIT_MODE: u32 = 0x0040;
    const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_WRAP_AT_EOL_OUTPUT: u32 = 0x0002;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;
    const CP_UTF8: u32 = 65001;

    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;
    const WAIT_FAILED: u32 = 0xFFFF_FFFF;
    const INFINITE: u32 = 0xFFFF_FFFF;

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    const CTRL_CLOSE_EVENT: u32 = 2;
    const CTRL_LOGOFF_EVENT: u32 = 5;
    const CTRL_SHUTDOWN_EVENT: u32 = 6;

    const ERROR_BROKEN_PIPE: u32 = 109;
    const ERROR_NO_DATA: u32 = 232;
    const ERROR_OPERATION_ABORTED: u32 = 995;
    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

    const GOT_WINCH: u32 = 1;
    const GOT_QUIT: u32 = 2;

    static WAKE_HANDLE: AtomicIsize = AtomicIsize::new(0);
    static INPUT_EVENT: AtomicIsize = AtomicIsize::new(0);
    static RESIZE_STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
    static READER_THREAD: AtomicIsize = AtomicIsize::new(0);
    static STOP_READER: AtomicBool = AtomicBool::new(false);
    static CTRL_INSTALLED: AtomicBool = AtomicBool::new(false);
    static PANIC_HOOK_SET: AtomicBool = AtomicBool::new(false);
    static PENDING: AtomicU32 = AtomicU32::new(0);
    static LAST_SIZE: AtomicU64 = AtomicU64::new(0);
    static GRAPHEME_TO_RESTORE: AtomicBool = AtomicBool::new(false);
    static THEME_MODE_TO_RESTORE: AtomicBool = AtomicBool::new(false);
    static RESTORE: Mutex<Option<Saved>> = Mutex::new(None);
    static INPUT_QUEUE: Mutex<InputQueue> = Mutex::new(InputQueue::new());

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    struct InputQueue {
        bytes: VecDeque<u8>,
        eof: bool,
    }

    impl InputQueue {
        const fn new() -> Self {
            Self {
                bytes: VecDeque::new(),
                eof: false,
            }
        }
    }

    struct Saved {
        in_handle: isize,
        out_handle: isize,
        orig_in: u32,
        orig_out: u32,
        orig_in_cp: u32,
        orig_out_cp: u32,
    }

    unsafe extern "system" {
        fn GetStdHandle(std_handle: u32) -> isize;
        fn GetConsoleMode(console_handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(console_handle: isize, mode: u32) -> i32;
        fn GetConsoleCP() -> u32;
        fn SetConsoleCP(code_page_id: u32) -> i32;
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleOutputCP(code_page_id: u32) -> i32;
        fn GetConsoleScreenBufferInfo(
            console_output: isize,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
        fn ReadConsoleW(
            console_input: isize,
            buffer: *mut u16,
            chars_to_read: u32,
            chars_read: *mut u32,
            input_control: *const c_void,
        ) -> i32;
        fn WriteFile(
            file: isize,
            buffer: *const u8,
            bytes_to_write: u32,
            bytes_written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CreateEventW(
            attributes: *const c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> isize;
        fn SetEvent(handle: isize) -> i32;
        fn ResetEvent(handle: isize) -> i32;
        fn CloseHandle(handle: isize) -> i32;
        fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
        fn WaitForMultipleObjects(
            count: u32,
            handles: *const isize,
            wait_all: i32,
            milliseconds: u32,
        ) -> u32;
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
        fn GetCurrentProcess() -> isize;
        fn GetCurrentThread() -> isize;
        fn DuplicateHandle(
            source_process: isize,
            source_handle: isize,
            target_process: isize,
            target_handle: *mut isize,
            desired_access: u32,
            inherit_handle: i32,
            options: u32,
        ) -> i32;
        fn CancelSynchronousIo(thread_handle: isize) -> i32;
        fn GetLastError() -> u32;
    }

    pub struct Wake {
        event: isize,
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct Waker(isize);

    impl Wake {
        pub fn new() -> io::Result<Self> {
            // SAFETY: creates an unnamed manual-reset Win32 Event object.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { event })
        }

        pub fn waker(&self) -> Waker {
            Waker(self.event)
        }

        pub fn drain(&self) {
            // SAFETY: `self.event` is a valid event owned by `self`.
            unsafe {
                ResetEvent(self.event);
            }
        }
    }

    impl Drop for Wake {
        fn drop(&mut self) {
            let _ = WAKE_HANDLE.compare_exchange(
                self.event,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            // SAFETY: `self.event` was created by `CreateEventW` and is owned uniquely by `self`.
            unsafe {
                CloseHandle(self.event);
            }
        }
    }

    impl Waker {
        pub fn wake(self) {
            if self.0 != 0 {
                // SAFETY: `self.0` is a Win32 Event handle when non-zero.
                unsafe {
                    SetEvent(self.0);
                }
            }
        }
    }

    fn wake_registered() {
        let h = WAKE_HANDLE.load(Ordering::Relaxed);
        if h != 0 {
            Waker(h).wake();
        }
    }

    fn timeout_ms(timeout: Option<Duration>) -> u32 {
        match timeout {
            None => INFINITE,
            Some(dur) if dur.is_zero() => 0,
            Some(dur) => {
                let ms = dur.as_millis().min(u128::from(u32::MAX - 1)) as u32;
                ms.max(1)
            }
        }
    }

    pub fn wait(wake: &Wake, timeout: Option<Duration>) -> io::Result<Ready> {
        let input_ev = INPUT_EVENT.load(Ordering::Acquire);
        let ms = timeout_ms(timeout);
        if input_ev != 0 {
            let handles = [input_ev, wake.event];
            // SAFETY: both handles are valid Win32 Event handles.
            let rc = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, ms) };
            if rc == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            let input_ready = unsafe { WaitForSingleObject(input_ev, 0) } == WAIT_OBJECT_0;
            let wake_ready = unsafe { WaitForSingleObject(wake.event, 0) } == WAIT_OBJECT_0;
            return Ok(Ready {
                input: input_ready,
                wake: wake_ready,
            });
        }
        // SAFETY: `wake.event` is a valid Win32 Event handle.
        let rc = unsafe { WaitForSingleObject(wake.event, ms) };
        if rc == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if rc == WAIT_TIMEOUT {
            return Ok(Ready::default());
        }
        Ok(Ready {
            input: false,
            wake: rc == WAIT_OBJECT_0,
        })
    }

    pub(crate) fn utf8_safe_chunk(data: &[u8], max_bytes: usize) -> usize {
        if data.len() <= max_bytes {
            return data.len();
        }
        let mut end = max_bytes;
        // Never split a multi-byte UTF-8 scalar across two WriteFile calls to the Windows console:
        // conhost decodes each WriteFile independently in CP_UTF8 mode.
        let floor = end.saturating_sub(3);
        while end > floor && (data[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        if end == 0 { max_bytes } else { end }
    }

    fn write_handle(handle: isize, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let chunk = utf8_safe_chunk(data, 64 * 1024) as u32;
            let mut written: u32 = 0;
            // SAFETY: `data` is valid for `chunk` bytes.
            let ok = unsafe {
                WriteFile(
                    handle,
                    data.as_ptr(),
                    chunk,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA {
                    return Err(io::Error::new(ErrorKind::BrokenPipe, "pipe closed"));
                }
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            if written == 0 {
                return Err(io::Error::new(ErrorKind::WriteZero, "short write to console"));
            }
            data = &data[written as usize..];
        }
        Ok(())
    }

    pub fn write_all_fd(fd: i32, data: &[u8]) -> io::Result<()> {
        let std_id = match fd {
            1 => STD_OUTPUT_HANDLE,
            2 => STD_ERROR_HANDLE,
            _ => {
                return Err(io::Error::new(
                    ErrorKind::Unsupported,
                    "writing to a descriptor: not supported yet on Windows",
                ));
            }
        };
        // SAFETY: `GetStdHandle` returns the process standard handle or INVALID_HANDLE_VALUE.
        let handle = unsafe { GetStdHandle(std_id) };
        if handle == 0 || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        write_handle(handle, data)
    }

    pub fn write_stdout(data: &[u8]) -> io::Result<()> {
        write_all_fd(1, data)
    }

    unsafe extern "system" fn on_ctrl(ctrl_type: u32) -> i32 {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                PENDING.fetch_or(GOT_QUIT, Ordering::Relaxed);
                wake_registered();
                1
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                restore_now();
                PENDING.fetch_or(GOT_QUIT, Ordering::Relaxed);
                wake_registered();
                0
            }
            _ => 0,
        }
    }

    pub fn install_signals(waker: Waker) -> io::Result<()> {
        WAKE_HANDLE.store(waker.0, Ordering::SeqCst);
        if !CTRL_INSTALLED.swap(true, Ordering::SeqCst) {
            // SAFETY: `on_ctrl` is a static `extern "system"` callback.
            if unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } == 0 {
                CTRL_INSTALLED.store(false, Ordering::SeqCst);
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn pack_size((cols, rows): (usize, usize)) -> u64 {
        ((cols as u64) << 32) | (rows as u32 as u64)
    }

    pub fn take_signals() -> Signals {
        if RESTORE.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            if let Ok(now) = window_size() {
                let packed = pack_size(now);
                let prev = LAST_SIZE.swap(packed, Ordering::Relaxed);
                if prev != 0 && prev != packed {
                    PENDING.fetch_or(GOT_WINCH, Ordering::Relaxed);
                }
            }
        }
        let pending = PENDING.swap(0, Ordering::SeqCst);
        Signals {
            resized: pending & GOT_WINCH != 0,
            quit: pending & GOT_QUIT != 0,
        }
    }

    pub struct Restorer;

    impl Drop for Restorer {
        fn drop(&mut self) {
            restore_now();
        }
    }

    fn restore_now() {
        let saved = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(saved) = saved else { return };
        STOP_READER.store(true, Ordering::SeqCst);
        let resize_stop = RESIZE_STOP_EVENT.swap(0, Ordering::SeqCst);
        if resize_stop != 0 {
            // SAFETY: signal resize loop to stop; the resize thread closes or detaches safely.
            unsafe {
                SetEvent(resize_stop);
            }
        }
        let reader_thread = READER_THREAD.swap(0, Ordering::SeqCst);
        if reader_thread != 0 {
            // SAFETY: unblocks pending `ReadConsoleW` on the console reader thread, then closes handle.
            unsafe {
                CancelSynchronousIo(reader_thread);
                CloseHandle(reader_thread);
            }
        }
        if GRAPHEME_TO_RESTORE.swap(false, Ordering::SeqCst) {
            let _ = write_handle(saved.out_handle, GRAPHEME_OFF);
        }
        if THEME_MODE_TO_RESTORE.swap(false, Ordering::SeqCst) {
            let _ = write_handle(saved.out_handle, THEME_MODE_OFF);
        }
        let _ = write_handle(saved.out_handle, LEAVE_TERM);
        // SAFETY: restores the exact console modes and codepages captured in `enter_raw`.
        unsafe {
            SetConsoleMode(saved.in_handle, saved.orig_in);
            SetConsoleMode(saved.out_handle, saved.orig_out);
            SetConsoleOutputCP(saved.orig_out_cp);
            SetConsoleCP(saved.orig_in_cp);
        }
    }

    pub(crate) fn push_utf16(
        out: &mut Vec<u8>,
        high_surrogate: &mut Option<u16>,
        units: &[u16],
    ) {
        for &u in units {
            if let Some(hi) = high_surrogate.take() {
                if (0xDC00..=0xDFFF).contains(&u) {
                    let cp = 0x10000 + (((u32::from(hi) - 0xD800) << 10) | (u32::from(u) - 0xDC00));
                    if let Some(ch) = char::from_u32(cp) {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    continue;
                }
            }
            if (0xD800..=0xDBFF).contains(&u) {
                *high_surrogate = Some(u);
            } else if let Some(ch) = char::from_u32(u32::from(u)) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }

    fn spawn_input_reader(in_handle: isize, input_event: isize) -> io::Result<()> {
        std::thread::Builder::new()
            .name("loupe-console-input".into())
            .spawn(move || {
                let mut dup_thread: isize = 0;
                // SAFETY: duplicates current thread pseudo-handle into a real handle for CancelSynchronousIo.
                unsafe {
                    let proc = GetCurrentProcess();
                    if DuplicateHandle(
                        proc,
                        GetCurrentThread(),
                        proc,
                        &mut dup_thread,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    ) != 0
                    {
                        READER_THREAD.store(dup_thread, Ordering::SeqCst);
                    }
                }
                let mut utf16 = [0u16; 1024];
                let mut high_surrogate: Option<u16> = None;
                let mut scratch = Vec::with_capacity(2048);
                while !STOP_READER.load(Ordering::Acquire) {
                    let mut read: u32 = 0;
                    // SAFETY: `utf16` is writable for `utf16.len()` UTF-16 units.
                    let ok = unsafe {
                        ReadConsoleW(
                            in_handle,
                            utf16.as_mut_ptr(),
                            utf16.len() as u32,
                            &mut read,
                            std::ptr::null(),
                        )
                    };
                    if STOP_READER.load(Ordering::Acquire) {
                        break;
                    }
                    if ok == 0 {
                        let err = unsafe { GetLastError() };
                        if err == ERROR_OPERATION_ABORTED && STOP_READER.load(Ordering::Acquire) {
                            break;
                        }
                        let mut q = INPUT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                        q.eof = true;
                        unsafe {
                            SetEvent(input_event);
                        }
                        break;
                    }
                    if read == 0 {
                        let mut q = INPUT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                        q.eof = true;
                        unsafe {
                            SetEvent(input_event);
                        }
                        break;
                    }
                    scratch.clear();
                    push_utf16(&mut scratch, &mut high_surrogate, &utf16[..read as usize]);
                    if !scratch.is_empty() {
                        let mut q = INPUT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                        q.bytes.extend(&scratch);
                        unsafe {
                            SetEvent(input_event);
                        }
                    }
                }
                let old = READER_THREAD.swap(0, Ordering::SeqCst);
                if old != 0 {
                    unsafe {
                        CloseHandle(old);
                    }
                }
            })?;
        Ok(())
    }

    fn spawn_resize_watcher(stop_event: isize, initial: (usize, usize)) -> io::Result<()> {
        std::thread::Builder::new()
            .name("loupe-console-resize".into())
            .spawn(move || {
                let mut last = initial;
                loop {
                    // SAFETY: `stop_event` stays open until this thread finishes and closes it.
                    let rc = unsafe { WaitForSingleObject(stop_event, 100) };
                    if rc == WAIT_OBJECT_0 || STOP_READER.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(now) = window_size() {
                        if now != last {
                            last = now;
                            LAST_SIZE.store(pack_size(now), Ordering::Relaxed);
                            PENDING.fetch_or(GOT_WINCH, Ordering::Relaxed);
                            wake_registered();
                        }
                    }
                }
                unsafe {
                    CloseHandle(stop_event);
                }
            })?;
        Ok(())
    }

    pub fn enter_raw() -> io::Result<Restorer> {
        // SAFETY: query standard handles and original console modes/codepages.
        let (in_handle, out_handle, orig_in, orig_out, orig_in_cp, orig_out_cp) = unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if in_handle == 0
                || in_handle == INVALID_HANDLE_VALUE
                || out_handle == 0
                || out_handle == INVALID_HANDLE_VALUE
            {
                return Err(io::Error::last_os_error());
            }
            let mut orig_in: u32 = 0;
            let mut orig_out: u32 = 0;
            if GetConsoleMode(in_handle, &mut orig_in) == 0 {
                return Err(io::Error::last_os_error());
            }
            if GetConsoleMode(out_handle, &mut orig_out) == 0 {
                return Err(io::Error::last_os_error());
            }
            let orig_in_cp = GetConsoleCP();
            let orig_out_cp = GetConsoleOutputCP();
            (
                in_handle,
                out_handle,
                orig_in,
                orig_out,
                orig_in_cp,
                orig_out_cp,
            )
        };

        let no_wrap_out = (orig_out & !ENABLE_WRAP_AT_EOL_OUTPUT)
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        let raw_out = no_wrap_out | DISABLE_NEWLINE_AUTO_RETURN;
        // SAFETY: enables VT sequences on stdout with EOL wrap disabled to prevent conhost scroll
        // corruption, falling back cleanly on older console hosts.
        unsafe {
            if SetConsoleMode(out_handle, raw_out) == 0
                && SetConsoleMode(out_handle, no_wrap_out) == 0
                && SetConsoleMode(out_handle, orig_out | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0
            {
                return Err(io::Error::last_os_error());
            }
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }

        let base_in = (orig_in
            & !(ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE))
            | ENABLE_EXTENDED_FLAGS
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        // SAFETY: enables raw VT keyboard + mouse input; rolls back output mode on failure.
        let in_ok = unsafe {
            SetConsoleMode(in_handle, base_in | ENABLE_MOUSE_INPUT) != 0
                || SetConsoleMode(in_handle, base_in) != 0
        };
        if !in_ok {
            let err = io::Error::last_os_error();
            unsafe {
                SetConsoleMode(out_handle, orig_out);
                SetConsoleOutputCP(orig_out_cp);
                SetConsoleCP(orig_in_cp);
            }
            return Err(err);
        }

        let input_event = {
            let existing = INPUT_EVENT.load(Ordering::Acquire);
            if existing != 0 {
                unsafe {
                    ResetEvent(existing);
                }
                existing
            } else {
                let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
                if ev == 0 {
                    let err = io::Error::last_os_error();
                    unsafe {
                        SetConsoleMode(in_handle, orig_in);
                        SetConsoleMode(out_handle, orig_out);
                        SetConsoleOutputCP(orig_out_cp);
                        SetConsoleCP(orig_in_cp);
                    }
                    return Err(err);
                }
                INPUT_EVENT.store(ev, Ordering::Release);
                ev
            }
        };
        {
            let mut q = INPUT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            q.bytes.clear();
            q.eof = false;
        }
        STOP_READER.store(false, Ordering::SeqCst);

        let resize_stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if resize_stop == 0 {
            let err = io::Error::last_os_error();
            unsafe {
                SetConsoleMode(in_handle, orig_in);
                SetConsoleMode(out_handle, orig_out);
                SetConsoleOutputCP(orig_out_cp);
                SetConsoleCP(orig_in_cp);
            }
            return Err(err);
        }
        RESIZE_STOP_EVENT.store(resize_stop, Ordering::SeqCst);

        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Saved {
            in_handle,
            out_handle,
            orig_in,
            orig_out,
            orig_in_cp,
            orig_out_cp,
        });

        if !PANIC_HOOK_SET.swap(true, Ordering::SeqCst) {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_now();
                prev(info);
            }));
        }

        let initial_size = window_size().unwrap_or((80, 24));
        LAST_SIZE.store(pack_size(initial_size), Ordering::Relaxed);

        if let Err(err) = spawn_input_reader(in_handle, input_event) {
            restore_now();
            return Err(err);
        }
        if let Err(err) = spawn_resize_watcher(resize_stop, initial_size) {
            restore_now();
            return Err(err);
        }
        if let Err(err) = write_handle(out_handle, ENTER_TERM)
            .and_then(|()| write_handle(out_handle, b"\x1b[2J\x1b[H"))
        {
            restore_now();
            return Err(err);
        }
        Ok(Restorer)
    }

    pub fn restore_grapheme_mode_on_exit() {
        GRAPHEME_TO_RESTORE.store(true, Ordering::SeqCst);
    }

    pub fn restore_theme_mode_on_exit() {
        THEME_MODE_TO_RESTORE.store(true, Ordering::SeqCst);
    }

    pub fn window_size() -> io::Result<(usize, usize)> {
        // SAFETY: queries stdout's console window dimensions.
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle == 0 || handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut info = ConsoleScreenBufferInfo::default();
            if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
                return Err(io::Error::last_os_error());
            }
            let cols = (i32::from(info.window.right) - i32::from(info.window.left) + 1).max(1);
            let rows = (i32::from(info.window.bottom) - i32::from(info.window.top) + 1).max(1);
            Ok((cols as usize, rows as usize))
        }
    }

    pub fn read_input() -> io::Result<(Vec<u8>, bool)> {
        let mut q = INPUT_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        let bytes: Vec<u8> = q.bytes.drain(..).collect();
        let eof = q.eof;
        if !eof {
            let ev = INPUT_EVENT.load(Ordering::Acquire);
            if ev != 0 {
                // SAFETY: called under INPUT_QUEUE lock so SetEvent cannot race with ResetEvent.
                unsafe {
                    ResetEvent(ev);
                }
            }
        }
        Ok((bytes, eof))
    }
}

#[cfg(windows)]
pub use win32::{
    Restorer, Wake, Waker, enter_raw, install_signals, read_input, restore_grapheme_mode_on_exit,
    restore_theme_mode_on_exit, take_signals, wait, window_size, write_all_fd, write_stdout,
};

#[cfg(all(windows, test))]
pub(crate) fn push_utf16_for_test(
    out: &mut Vec<u8>,
    high_surrogate: &mut Option<u16>,
    units: &[u16],
) {
    win32::push_utf16(out, high_surrogate, units);
}

#[cfg(all(windows, test))]
pub(crate) fn utf8_safe_chunk_for_test(data: &[u8], max_bytes: usize) -> usize {
    win32::utf8_safe_chunk(data, max_bytes)
}

#[cfg(not(windows))]
pub struct Wake(());

#[cfg(not(windows))]
#[derive(Clone, Copy, Debug, Default)]
pub struct Waker(());

#[cfg(not(windows))]
impl Wake {
    pub fn new() -> io::Result<Self> {
        Err(not_supported("the terminal presenter"))
    }
    pub fn waker(&self) -> Waker {
        Waker(())
    }
    pub fn drain(&self) {}
}

#[cfg(not(windows))]
impl Waker {
    pub fn wake(self) {}
}

#[cfg(not(windows))]
pub fn wait(_wake: &Wake, _timeout: Option<Duration>) -> io::Result<Ready> {
    Err(not_supported("waiting on the console"))
}

#[cfg(not(windows))]
pub fn write_all_fd(_fd: i32, _data: &[u8]) -> io::Result<()> {
    Err(not_supported("writing to a descriptor"))
}

#[cfg(not(windows))]
pub fn write_stdout(_data: &[u8]) -> io::Result<()> {
    Err(not_supported("writing to the console"))
}

#[cfg(not(windows))]
pub fn install_signals(_waker: Waker) -> io::Result<()> {
    Err(not_supported("signal handling"))
}

#[cfg(not(windows))]
pub fn take_signals() -> Signals {
    Signals::default()
}

#[cfg(not(windows))]
pub struct Restorer(());

#[cfg(not(windows))]
pub fn enter_raw() -> io::Result<Restorer> {
    Err(not_supported("raw console mode"))
}

#[cfg(not(windows))]
pub fn restore_grapheme_mode_on_exit() {}

#[cfg(not(windows))]
pub fn restore_theme_mode_on_exit() {}

#[cfg(not(windows))]
pub fn window_size() -> io::Result<(usize, usize)> {
    Err(not_supported("reading the console size"))
}

#[cfg(not(windows))]
pub fn read_input() -> io::Result<(Vec<u8>, bool)> {
    Err(not_supported("reading console input"))
}
