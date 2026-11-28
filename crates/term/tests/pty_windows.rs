#![cfg(windows)]
//! End-to-end ConPTY integration tests verifying that `loupe-term` enters raw VT mode,
//! handles probe replies, keys, resizes, and exits cleanly while restoring the terminal.

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[path = "../../core/tests/support/mod.rs"]
mod support;

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const INVALID_HANDLE_VALUE: isize = -1;
const WAIT_OBJECT_0: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    std_input: isize,
    std_output: isize,
    std_error: isize,
}

#[repr(C)]
struct StartupInfoExW {
    startup_info: StartupInfoW,
    attribute_list: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: isize,
    thread: isize,
    process_id: u32,
    thread_id: u32,
}

unsafe extern "system" {
    fn CreatePipe(
        read_pipe: *mut isize,
        write_pipe: *mut isize,
        security_attributes: *const c_void,
        size: u32,
    ) -> i32;
    fn CreatePseudoConsole(
        size: Coord,
        input: isize,
        output: isize,
        flags: u32,
        ph_pc: *mut isize,
    ) -> i32;
    fn ResizePseudoConsole(hpc: isize, size: Coord) -> i32;
    fn ClosePseudoConsole(hpc: isize);
    fn InitializeProcThreadAttributeList(
        attribute_list: *mut c_void,
        attribute_count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        attribute_list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *mut c_void,
        size: usize,
        previous_value: *mut c_void,
        return_size: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(attribute_list: *mut c_void);
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: isize, exit_code: *mut u32) -> i32;
    fn TerminateProcess(process: isize, exit_code: u32) -> i32;
    fn CloseHandle(handle: isize) -> i32;
}

struct ConPtySession {
    hpc: isize,
    process: isize,
    input_writer: Option<std::fs::File>,
    output: Arc<(Mutex<Vec<u8>>, Condvar)>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ConPtySession {
    fn spawn(cols: i16, rows: i16, args: &[&str]) -> io::Result<Self> {
        let (mut in_read, mut in_write) = (0isize, 0isize);
        let (mut out_read, mut out_write) = (0isize, 0isize);
        unsafe {
            if CreatePipe(&mut in_read, &mut in_write, std::ptr::null(), 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            if CreatePipe(&mut out_read, &mut out_write, std::ptr::null(), 0) == 0 {
                CloseHandle(in_read);
                CloseHandle(in_write);
                return Err(io::Error::last_os_error());
            }
        }

        let mut hpc = 0isize;
        let hr = unsafe { CreatePseudoConsole(Coord { x: cols, y: rows }, in_read, out_write, 0, &mut hpc) };
        if hr != 0 {
            unsafe {
                CloseHandle(in_read);
                CloseHandle(in_write);
                CloseHandle(out_read);
                CloseHandle(out_write);
            }
            return Err(io::Error::from_raw_os_error(hr));
        }

        let mut attr_size = 0usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
        }
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = attr_buf.as_mut_ptr().cast::<c_void>();
        if unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) } == 0 {
            let err = io::Error::last_os_error();
            unsafe {
                ClosePseudoConsole(hpc);
                CloseHandle(in_read);
                CloseHandle(in_write);
                CloseHandle(out_read);
                CloseHandle(out_write);
            }
            return Err(err);
        }
        if unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                hpc as *mut c_void,
                size_of::<isize>(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            let err = io::Error::last_os_error();
            unsafe {
                DeleteProcThreadAttributeList(attr_list);
                ClosePseudoConsole(hpc);
                CloseHandle(in_read);
                CloseHandle(in_write);
                CloseHandle(out_read);
                CloseHandle(out_write);
            }
            return Err(err);
        }

        let exe = std::path::Path::new(env!("CARGO_BIN_EXE_loupe-term"));
        let mut cmdline = format!("\"{}\"", exe.display());
        for a in args {
            cmdline.push(' ');
            cmdline.push_str(a);
        }
        let mut cmd_w: Vec<u16> = std::ffi::OsStr::new(&cmdline)
            .encode_wide()
            .chain(Some(0))
            .collect();

        let mut si: StartupInfoExW = unsafe { zeroed() };
        si.startup_info.cb = size_of::<StartupInfoExW>() as u32;
        // Clear inherited stdio from redirected test-runner pipes so ConPTY attaches its console handles.
        si.startup_info.flags = STARTF_USESTDHANDLES;
        si.startup_info.std_input = INVALID_HANDLE_VALUE;
        si.startup_info.std_output = INVALID_HANDLE_VALUE;
        si.startup_info.std_error = INVALID_HANDLE_VALUE;
        si.attribute_list = attr_list;
        let mut pi: ProcessInformation = unsafe { zeroed() };

        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd_w.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &si.startup_info,
                &mut pi,
            )
        };
        unsafe {
            DeleteProcThreadAttributeList(attr_list);
            CloseHandle(in_read);
            CloseHandle(out_write);
        }
        if ok == 0 {
            let err = io::Error::last_os_error();
            unsafe {
                ClosePseudoConsole(hpc);
                CloseHandle(in_write);
                CloseHandle(out_read);
            }
            return Err(err);
        }
        unsafe {
            CloseHandle(pi.thread);
        }

        let output = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let reader_out = Arc::clone(&output);
        let mut out_file = unsafe { std::fs::File::from_raw_handle(out_read as RawHandle) };
        let reader_thread = std::thread::spawn(move || {
            let mut tmp = [0u8; 4096];
            loop {
                match out_file.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let (lock, cv) = &*reader_out;
                        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        guard.extend_from_slice(&tmp[..n]);
                        cv.notify_all();
                    }
                }
            }
        });
        let input_writer = unsafe { std::fs::File::from_raw_handle(in_write as RawHandle) };

        Ok(Self {
            hpc,
            process: pi.process,
            input_writer: Some(input_writer),
            output,
            reader_thread: Some(reader_thread),
        })
    }

    fn send(&mut self, bytes: &[u8]) {
        let w = self.input_writer.as_mut().unwrap();
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    fn resize(&self, cols: i16, rows: i16) {
        let hr = unsafe { ResizePseudoConsole(self.hpc, Coord { x: cols, y: rows }) };
        assert_eq!(hr, 0, "ResizePseudoConsole failed: {hr:#x}");
    }

    fn wait_for_bytes(&self, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let (lock, cv) = &*self.output;
        let mut guard = lock.lock().unwrap();
        loop {
            if guard.windows(needle.len()).any(|w| w == needle) {
                return guard.clone();
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                panic!(
                    "timed out waiting for {:?} in {} bytes of ConPTY output:\n{}",
                    String::from_utf8_lossy(needle),
                    guard.len(),
                    String::from_utf8_lossy(&guard)
                );
            };
            let (g, _) = cv.wait_timeout(guard, left).unwrap();
            guard = g;
        }
    }

    fn wait_exit(&mut self, timeout: Duration) -> u32 {
        let rc = unsafe { WaitForSingleObject(self.process, timeout.as_millis() as u32) };
        assert_eq!(rc, WAIT_OBJECT_0, "process did not exit within {timeout:?}");
        let mut code = 0u32;
        assert_ne!(unsafe { GetExitCodeProcess(self.process, &mut code) }, 0);
        // Drop input handle & close pseudo console so the reader thread hits EOF cleanly.
        self.input_writer.take();
        unsafe {
            ClosePseudoConsole(self.hpc);
        }
        self.hpc = 0;
        if let Some(t) = self.reader_thread.take() {
            let _ = t.join();
        }
        code
    }
}

impl Drop for ConPtySession {
    fn drop(&mut self) {
        self.input_writer.take();
        if self.process != 0 {
            let rc = unsafe { WaitForSingleObject(self.process, 50) };
            if rc != WAIT_OBJECT_0 {
                unsafe {
                    TerminateProcess(self.process, 1);
                    WaitForSingleObject(self.process, 500);
                }
            }
            unsafe {
                CloseHandle(self.process);
            }
            self.process = 0;
        }
        if self.hpc != 0 {
            unsafe {
                ClosePseudoConsole(self.hpc);
            }
            self.hpc = 0;
        }
        if let Some(t) = self.reader_thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn conpty_renders_dashboard_handles_keys_resize_and_restores_on_quit() {
    let mut pty = ConPtySession::spawn(110, 32, &["--demo", "--calm"]).unwrap();
    // ConPTY replies to terminal queries automatically, and the dashboard renders directly.
    pty.wait_for_bytes(b"Plan the search index rebuild", Duration::from_secs(5));

    // Exercise VT arrow key input, then view switches (`i` Insights, `e` Telemetry, `w` Trace).
    pty.send(b"\x1b[B");
    pty.send(b"i");
    pty.wait_for_bytes(b"RECAP", Duration::from_secs(5));

    pty.send(b"e");
    pty.wait_for_bytes(b"TELEMETRY", Duration::from_secs(5));

    pty.send(b"w");
    pty.wait_for_bytes(b"TRACE", Duration::from_secs(5));

    pty.resize(84, 24);
    std::thread::sleep(Duration::from_millis(250));

    pty.send(b"q");
    let code = pty.wait_exit(Duration::from_secs(5));
    assert_eq!(code, 0, "expected clean exit on q");
}

#[test]
fn conpty_ctrl_c_exits_cleanly_and_restores_terminal() {
    let mut pty = ConPtySession::spawn(100, 28, &["--demo", "--calm"]).unwrap();
    pty.wait_for_bytes(b"Plan the search index rebuild", Duration::from_secs(5));

    pty.send(b"\x03");
    let code = pty.wait_exit(Duration::from_secs(5));
    assert_eq!(code, 0, "expected clean exit on Ctrl+C");
}

#[test]
fn conpty_renders_the_reference_dashboard_at_270_by_61() {
    let mut pty = ConPtySession::spawn(270, 61, &["--demo", "--calm", "--theme", "kanagawa"]).unwrap();
    for part in ["LIMITS", "PLAN", "ACTIVITY", "GROWTH", "PROMPT CACHE", "╝", "demo data"] {
        pty.wait_for_bytes(part.as_bytes(), Duration::from_secs(5));
    }
    pty.send(b"q");
    assert_eq!(pty.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn conpty_updates_the_token_tile_when_t3_records_a_turn_total() {
    let db = support::Db::new();
    db.db.execute_batch(r#"
        INSERT INTO orchestration_v2_projection_runs
          (run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,payload_json)
          VALUES ('run','thread',1,'new-provider','provider','completed','',
                  '{"modelSelection":{"model":"new-provider/long-future-model-v7"}}');
        INSERT INTO orchestration_v2_projection_nodes
          (node_id,thread_id,run_id,root_node_id,kind,status,provider_thread_id,provider_turn_id,payload_json)
          VALUES ('node','thread','run','node','provider_turn','completed','provider','turn','{}');
    "#).unwrap();
    let path = format!("\"{}\"", db.path.display());
    let mut pty = ConPtySession::spawn(
        270,
        61,
        &["--db", &path, "--interval", "1", "--calm", "--theme", "kanagawa"],
    ).unwrap();
    pty.wait_for_bytes(b"no token totals reported", Duration::from_secs(5));
    let completed = loupe_core::recap::iso(loupe_core::clock::wall_now());
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_provider_turns
         (provider_turn_id, thread_id, provider_thread_id, node_id, ordinal, status, completed_at, payload_json)
         VALUES ('turn','thread','provider','node',1,'completed',?1,?2)",
        rusqlite::params![completed, r#"{"turnTokenUsage":{"usageStatus":"complete","inputTokens":60000,"outputTokens":2000}}"#],
    ).unwrap();
    pty.wait_for_bytes(b"other 62.0K", Duration::from_secs(10));
    pty.wait_for_bytes(b"new-provider/long-future-model-v7", Duration::from_secs(5));
    pty.wait_for_bytes(b"T3 Code turn totals", Duration::from_secs(5));
    pty.send(b"q");
    assert_eq!(pty.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn conpty_compares_models_projects_and_historical_periods() {
    let timeout = Duration::from_secs(8);
    let mut pty = ConPtySession::spawn(150, 42, &["--demo", "--calm"]).unwrap();
    pty.wait_for_bytes(b"Plan the search index rebuild", timeout);
    pty.send(b"i");
    pty.wait_for_bytes(b"v Compare", timeout);
    pty.send(b"v");
    pty.wait_for_bytes(b"INSIGHTS COMPARE", timeout);
    pty.wait_for_bytes(b"TOK/RUN (N)", timeout);
    pty.wait_for_bytes(b"gemini-4-neon", timeout);
    pty.send(b"\t");
    pty.wait_for_bytes(b"Billing migration", timeout);
    pty.wait_for_bytes(b"Search index", timeout);
    pty.send(b"[");
    pty.wait_for_bytes(b"Completed periods", timeout);
    pty.send(b"q");
    assert_eq!(pty.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn conpty_shows_usage_history_and_completed_compaction() {
    let mut db = support::Db::new();
    db.write("thread", 1);
    let now = loupe_core::clock::wall_now();
    let stamp = loupe_core::recap::iso(now);
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_provider_threads
         (provider_thread_id,thread_id,provider,status,updated_at,payload_json)
         VALUES('provider','thread','codex','running',?1,?2)",
        rusqlite::params![stamp, r#"{"id":"provider","contextUsage":{"usedTokens":84000,"maxTokens":475000}}"#],
    ).unwrap();
    db.db.execute(
        "UPDATE orchestration_v2_projection_threads SET active_provider_thread_id='provider',updated_at=?1
         WHERE thread_id='thread'", [&stamp],
    ).unwrap();
    let started = loupe_core::recap::iso(now - 1000.);
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_runs
         (run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,completed_at,payload_json)
         VALUES ('run','thread',1,'codex','provider','completed',?1,?2,?3)",
        rusqlite::params![started, stamp,
            serde_json::json!({"startedAt":started, "status":"completed"}).to_string()],
    ).unwrap();
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_provider_turns
         (provider_turn_id, thread_id, provider_thread_id, node_id, ordinal, status, completed_at, payload_json)
         VALUES ('turn','thread','provider','node',1,'completed',?1,?2)",
        rusqlite::params![stamp, r#"{"model":"new-future-model","turnTokenUsage":{"inputTokens":4000000,"outputTokens":900000}}"#],
    ).unwrap();
    let row = serde_json::json!([now, 0, 0, 100000, 84000000, 50000, 150000, 0, "receipt", null, 0]);
    let older = serde_json::json!([now - 9. * 86400000., 0, 0, 50000000, 0, 0, 0, 0, "older", null, 0]);
    let cache = serde_json::json!({
        "version":17, "models":["new-future-model"], "sessions":["session"],
        "files":{"example":{"p":"codex","r":[row, older],"t":[row]}}
    });
    std::fs::write(db.path.with_file_name("usage-scan-cache-v17.json"), cache.to_string()).unwrap();
    let path = format!("\"{}\"", db.path.display());
    let mut pty = ConPtySession::spawn(200, 52, &["--db", &path, "--calm"]).unwrap();
    let timeout = Duration::from_secs(8);
    pty.wait_for_bytes(b"codex 84.3M", timeout);
    pty.wait_for_bytes("Chat compactions · 0 recorded".as_bytes(), timeout);
    pty.wait_for_bytes("Project compactions · 0 recorded".as_bytes(), timeout);
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_turn_items
         (turn_item_id,thread_id,provider_thread_id,ordinal,type,status,updated_at,payload_json)
         VALUES ('compaction','thread','provider',1,'compaction','completed',?1,?2)",
        rusqlite::params![stamp, r#"{"beforeTokenCount":313000,"afterTokenCount":84000}"#],
    ).unwrap();
    pty.wait_for_bytes(b"229K tokens freed", Duration::from_secs(12));
    pty.send(b"i");
    pty.wait_for_bytes(b"TOTAL TOKENS", timeout);
    pty.wait_for_bytes(b"TURN TOKENS", timeout);
    pty.resize(84, 28);
    pty.wait_for_bytes(b"TOTAL TOKENS   84.3M", timeout);
    pty.resize(200, 52);
    pty.wait_for_bytes(b"v Compare", timeout);
    pty.send(b"v");
    pty.wait_for_bytes(b"Reported 84.3M", timeout);
    pty.wait_for_bytes(b"Cache write 50.0K", timeout);
    pty.send(b"q");
    assert_eq!(pty.wait_exit(Duration::from_secs(5)), 0);
}

#[test]
fn conpty_keeps_last_month_selected_with_usage_and_no_saved_runs() {
    use loupe_core::{
        clock, platform,
        product::{self, Database, Origin},
        recap::{self, InsightsUsage, Period, Which},
        recap_view,
    };

    let mut db = support::Db::new();
    db.write("thread", 1);
    let now = clock::wall_now();
    let start = recap::iso(now - 2.);
    let end = recap::iso(now - 1.);
    let payload = serde_json::json!({"startedAt":start, "status":"completed"});
    db.db.execute(
        "INSERT INTO orchestration_v2_projection_runs
         (run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json)
         VALUES ('current','thread',1,'codex','completed',?1,?2,?3)",
        rusqlite::params![start, end, payload.to_string()],
    ).unwrap();

    let local = &platform::local_time;
    let previous = Period::new(now, Which::LastMonth, local);
    let row = serde_json::json!([
        previous.start() + 3600000., 0, 0, 10000000, 39000000, 0, 1000000, 0, "old", null, 0
    ]);
    let cache = serde_json::json!({
        "version":17, "models":["codex-model"], "sessions":["old-session"],
        "files":{"example":{"p":"codex","r":[row]}}
    });
    std::fs::write(db.path.with_file_name("usage-scan-cache-v17.json"), cache.to_string()).unwrap();
    let database = Database::Path { path: db.path.clone(), origin: Origin::Db };
    let cli = product::load_recap(&database, now, Which::LastMonth, local).unwrap();
    assert_eq!(cli.returned, Which::ThisMonth, "fixture must have current runs only");
    let pending = product::load_insights_recap(&database, now, Which::LastMonth, local).unwrap();
    assert_eq!(pending.returned, Which::LastMonth);
    assert_eq!(pending.value.insights_usage, Some(InsightsUsage::Pending));
    assert_eq!(pending.value.runs, 0);
    let dates = recap_view::period_text(pending.value.period, &pending.value.from, &pending.value.to);

    let path = format!("\"{}\"", db.path.display());
    let mut pty = ConPtySession::spawn(200, 52, &["--db", &path, "--calm"]).unwrap();
    let timeout = Duration::from_secs(10);
    pty.wait_for_bytes(b"Loupe", timeout);
    pty.send(b"i");
    pty.wait_for_bytes(b"RECAP", timeout);
    pty.send(b"\x1b[C\x1b[C\x1b[C");
    pty.wait_for_bytes(dates.as_bytes(), timeout);
    for text in [
        "AGENT HOURS", "MOST AT ONCE", "no saved runs", "TOTAL TOKENS",
        "TOKENS BY DAY", "WHERE THE HOURS WENT", "models · by tokens",
        "codex-model", "50.0M", "HANDED TO AGENTS", "39.0M", "cached input",
    ] {
        pty.wait_for_bytes(text.as_bytes(), timeout);
    }
    pty.resize(84, 29);
    pty.wait_for_bytes(b"TOTAL TOKENS   50.0M", timeout);
    pty.wait_for_bytes(b"tokens by week", timeout);
    pty.resize(70, 29);
    pty.wait_for_bytes(b"Tokens by day", timeout);
    pty.send(b"q");
    assert_eq!(pty.wait_exit(Duration::from_secs(5)), 0);
}
