//! Windows notification area watcher. The window is hidden and owns the tray icon.
use loupe_core::{
    integration, live,
    model::{self, Agent, Snapshot},
    terminal,
};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    ffi::c_void,
    io,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    ptr,
    sync::OnceLock,
    time::{Duration, Instant},
};

const WM_CLOSE: u32 = 0x0010;
const WM_CONTEXTMENU: u32 = 0x007b;
const WM_TRAY: u32 = 0x8001;
const WM_UPDATES: u32 = 0x8002;
const WM_SHELL_WAKE: u32 = 0x8003;
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;
const NIN_BALLOONHIDE: u32 = 0x0403;
const NIN_BALLOONTIMEOUT: u32 = 0x0404;
const NIN_BALLOONUSERCLICK: u32 = 0x0405;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 1;
const NIF_ICON: u32 = 2;
const NIF_TIP: u32 = 4;
const NIF_INFO: u32 = 0x10;
const NIF_REALTIME: u32 = 0x40;
const NIF_SHOWTIP: u32 = 0x80;
const NIIF_INFO: u32 = 1;
const NIIF_RESPECT_QUIET_TIME: u32 = 0x80;

type Handle = isize;

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct Message {
    hwnd: Handle,
    id: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    private: u32,
}

#[repr(C)]
struct WindowClass {
    style: u32,
    procedure: unsafe extern "system" fn(Handle, u32, usize, isize) -> isize,
    extra_class: i32,
    extra_window: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu_name: *const u16,
    name: *const u16,
}

#[repr(C)]
struct NotifyIconData {
    size: u32,
    hwnd: Handle,
    id: u32,
    flags: u32,
    callback: u32,
    icon: Handle,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    title: [u16; 64],
    info_flags: u32,
    guid: [u32; 4],
    balloon_icon: Handle,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> Handle;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetModuleHandleW(name: *const u16) -> Handle;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WindowClass) -> u16;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn CreateWindowExW(
        extra: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        data: *const c_void,
    ) -> Handle;
    fn DestroyWindow(hwnd: Handle) -> i32;
    fn DefWindowProcW(hwnd: Handle, message: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(message: *mut Message, hwnd: Handle, from: u32, to: u32) -> i32;
    fn DispatchMessageW(message: *const Message) -> isize;
    fn PostMessageW(hwnd: Handle, message: u32, wparam: usize, lparam: isize) -> i32;
    fn LoadIconW(instance: Handle, name: *const u16) -> Handle;
    fn LoadImageW(
        instance: Handle,
        name: *const u16,
        kind: u32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> Handle;
    fn DestroyIcon(icon: Handle) -> i32;
    fn CreatePopupMenu() -> Handle;
    fn AppendMenuW(menu: Handle, flags: u32, id: usize, title: *const u16) -> i32;
    fn DestroyMenu(menu: Handle) -> i32;
    fn TrackPopupMenu(
        menu: Handle,
        flags: u32,
        x: i32,
        y: i32,
        reserved: i32,
        owner: Handle,
        rect: *const c_void,
    ) -> u32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: Handle) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(message: u32, icon: *mut NotifyIconData) -> i32;
    fn ShellExecuteW(
        hwnd: Handle,
        verb: *const u16,
        path: *const u16,
        args: *const u16,
        directory: *const u16,
        show: i32,
    ) -> Handle;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn write_wide<T: AsRef<[u16]>>(dest: &mut [u16], text: T) {
    dest.fill(0);
    let text = text.as_ref();
    let mut len = text.len().min(dest.len() - 1);
    if len > 0 && (0xd800..=0xdbff).contains(&text[len - 1]) {
        len -= 1;
    }
    dest[..len].copy_from_slice(&text[..len]);
}

enum ShellEvent {
    Close,
    Open,
    Menu,
    NoticeHidden,
    NoticeClicked,
    TaskbarCreated,
}

static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
thread_local! {
    static SHELL_EVENTS: RefCell<VecDeque<ShellEvent>> = const { RefCell::new(VecDeque::new()) };
}

unsafe extern "system" fn window_proc(
    hwnd: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    let event = match message {
        WM_CLOSE => ShellEvent::Close,
        WM_TRAY => match lparam as u32 & 0xffff {
            WM_CONTEXTMENU => ShellEvent::Menu,
            NIN_SELECT | NIN_KEYSELECT => ShellEvent::Open,
            NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT => ShellEvent::NoticeHidden,
            NIN_BALLOONUSERCLICK => ShellEvent::NoticeClicked,
            _ => return 0,
        },
        id if id != 0 && TASKBAR_CREATED.get() == Some(&id) => ShellEvent::TaskbarCreated,
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    };
    // Sent messages and messages dispatched by a native menu land here too.
    SHELL_EVENTS.with_borrow_mut(|events| events.push_back(event));
    unsafe { PostMessageW(hwnd, WM_SHELL_WAKE, 0, 0) };
    0
}

struct Instance(Handle);

impl Instance {
    fn lock() -> io::Result<Option<Self>> {
        let name = wide("Local\\LoupeTray");
        // The kernel keeps the named mutex alive until its last handle is closed.
        let (handle, exists) = unsafe {
            let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
            (handle, GetLastError() == 183)
        };
        if handle == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((!exists).then_some(Self(handle)))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Window(Handle);

impl Window {
    fn create() -> io::Result<Self> {
        let name = wide("LoupeTrayWindow");
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let class = WindowClass {
            style: 0,
            procedure: window_proc,
            extra_class: 0,
            extra_window: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: ptr::null(),
            name: name.as_ptr(),
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // A hidden top-level window receives Explorer's TaskbarCreated broadcast.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                instance,
                ptr::null(),
            )
        };
        if hwnd == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(hwnd))
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

struct Icon {
    data: NotifyIconData,
    owns_icon: bool,
}

impl Icon {
    fn new(window: &Window) -> io::Result<Self> {
        let custom = loupe_core::platform::home_dir()
            .map(PathBuf::from)
            .map(|home| home.join(".config/loupe/loupe.ico"))
            .filter(|path| path.is_file())
            .map_or(0, |path| {
                let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
                // IMAGE_ICON with LR_LOADFROMFILE.
                unsafe { LoadImageW(0, path.as_ptr(), 1, 16, 16, 0x10) }
            });
        // These fields are integers, handles and UTF-16 buffers. Zero initializes each.
        let mut data: NotifyIconData = unsafe { std::mem::zeroed() };
        data.size = std::mem::size_of::<NotifyIconData>() as u32;
        data.hwnd = window.0;
        data.id = 1;
        data.callback = WM_TRAY;
        data.icon = if custom != 0 {
            custom
        } else {
            unsafe { LoadIconW(0, 32512usize as *const u16) }
        };
        write_wide(&mut data.tip, wide("Loupe · connecting to T3"));
        let mut icon = Self {
            data,
            owns_icon: custom != 0,
        };
        icon.add()?;
        Ok(icon)
    }

    fn add(&mut self) -> io::Result<()> {
        self.data.flags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        if unsafe { Shell_NotifyIconW(NIM_ADD, &mut self.data) } == 0 {
            return Err(io::Error::other(
                "Windows could not add the Loupe tray icon",
            ));
        }
        self.data.version = 4;
        if unsafe { Shell_NotifyIconW(NIM_SETVERSION, &mut self.data) } == 0 {
            return Err(io::Error::other(
                "Windows could not configure the Loupe tray icon",
            ));
        }
        Ok(())
    }

    fn tip(&mut self, text: &str) {
        let old = self.data.tip;
        write_wide(&mut self.data.tip, wide(text));
        if old != self.data.tip {
            self.data.flags = NIF_TIP | NIF_SHOWTIP;
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &mut self.data);
            }
        }
    }

    fn notify(&mut self, note: &Notice) -> bool {
        write_wide(&mut self.data.title, wide(note.title));
        write_wide(&mut self.data.info, wide(&note.thread_title));
        self.data.flags = NIF_INFO | NIF_REALTIME;
        self.data.info_flags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut self.data) != 0 }
    }

    fn dismiss(&mut self) {
        self.data.info.fill(0);
        self.data.flags = NIF_INFO;
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &mut self.data);
        }
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &mut self.data);
            if self.owns_icon {
                DestroyIcon(self.data.icon);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Notice {
    title: &'static str,
    thread_id: String,
    thread_title: String,
}

fn local_threads(snapshot: &Snapshot) -> impl Iterator<Item = &Agent> {
    snapshot
        .agents
        .iter()
        .map(|a| a.as_ref())
        .filter(|a| a.kind == "thread" && a.remote.is_none() && !a.thread_id.is_empty())
}

#[derive(Default)]
struct Alerts {
    waiting: Option<BTreeSet<String>>,
    watched: BTreeSet<String>,
}

impl Alerts {
    fn update(&mut self, snapshot: &Snapshot, queue: &mut VecDeque<Notice>) {
        let mut waiting = BTreeSet::new();
        for agent in local_threads(snapshot) {
            if model::needs_you(agent) {
                let first = waiting.insert(agent.thread_id.clone());
                if first
                    && self
                        .waiting
                        .as_ref()
                        .is_some_and(|old| !old.contains(&agent.thread_id))
                {
                    queue.push_back(note("Loupe · Needs your input", agent));
                }
            }
            let title = match agent.status.as_str() {
                "completed" => Some("Loupe · Watched thread finished"),
                "failed" => Some("Loupe · Watched thread failed"),
                "interrupted" | "cancelled" | "rolled_back" => {
                    Some("Loupe · Watched thread stopped")
                }
                _ => None,
            };
            if let Some(title) = title
                && self.watched.remove(&agent.thread_id)
            {
                queue.push_back(note(title, agent));
            }
        }
        self.waiting = Some(waiting);
    }

    fn toggle(&mut self, thread: String) {
        if !self.watched.remove(&thread) {
            self.watched.insert(thread);
        }
    }
}

fn note(title: &'static str, agent: &Agent) -> Notice {
    let text = terminal::clean(&agent.title);
    Notice {
        title,
        thread_id: agent.thread_id.clone(),
        thread_title: if text.trim().is_empty() {
            "T3 Code thread".into()
        } else {
            text
        },
    }
}

fn open_dashboard() {
    if let Some(path) = loupe_core::platform::home_dir()
        .map(PathBuf::from)
        .map(|home| home.join("Desktop/Loupe.lnk"))
        .filter(|path| path.is_file())
    {
        let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
        if unsafe { ShellExecuteW(0, ptr::null(), path.as_ptr(), ptr::null(), ptr::null(), 1) } > 32
        {
            return;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        // CREATE_NEW_CONSOLE. Use the same executable that owns the tray.
        let _ = std::process::Command::new(exe).creation_flags(0x10).spawn();
    }
}

enum MenuAction {
    Open,
    Toggle(String),
    Exit,
}

fn menu(window: &Window, snapshot: Option<&Snapshot>, alerts: &Alerts) -> Option<MenuAction> {
    let menu = unsafe { CreatePopupMenu() };
    if menu == 0 {
        return None;
    }
    let mut actions = vec![MenuAction::Open, MenuAction::Exit];
    let add = |id: usize, flags: u32, name: &str| {
        let label = wide(name);
        unsafe {
            AppendMenuW(menu, flags, id, label.as_ptr());
        }
    };
    add(1, 0, "Open Loupe");
    add(0, 0x800, "");
    let mut entries: Vec<_> = snapshot
        .into_iter()
        .flat_map(local_threads)
        .filter(|a| model::live(a) || alerts.watched.contains(&a.thread_id))
        .collect();
    entries.sort_by(|a, b| a.title.cmp(&b.title));
    if entries.is_empty() {
        add(0, 0x0001, "No running threads to watch");
    }
    for agent in entries {
        let watching = alerts.watched.contains(&agent.thread_id);
        let action = if watching { "Stop watching" } else { "Watch" };
        let title = terminal::fit(&terminal::clean(&agent.title), 64).replace('&', "&&");
        actions.push(MenuAction::Toggle(agent.thread_id.clone()));
        add(
            actions.len(),
            if watching { 0x0008 } else { 0 },
            &format!("{action} {title}"),
        );
    }
    add(0, 0x800, "");
    add(2, 0, "Exit Loupe Tray");
    let mut point = Point::default();
    let picked = unsafe {
        GetCursorPos(&mut point);
        SetForegroundWindow(window.0);
        // TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON.
        let picked = TrackPopupMenu(
            menu,
            0x0100 | 0x0080 | 0x0002,
            point.x,
            point.y,
            0,
            window.0,
            ptr::null(),
        );
        DestroyMenu(menu);
        PostMessageW(window.0, 0, 0, 0);
        picked
    };
    actions.into_iter().nth(picked.checked_sub(1)? as usize)
}

/// Start the opt-in Windows tray watcher. No console or dashboard is opened.
pub fn run_tray(path: &Path) -> io::Result<()> {
    let Some(_instance) = Instance::lock()? else {
        return Ok(());
    };
    TASKBAR_CREATED
        .get_or_init(|| unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) });
    let window = Window::create()?;
    let mut icon = Icon::new(&window)?;
    let hwnd = window.0;
    let watcher = live::spawn_status(path, Duration::from_secs(2), move || unsafe {
        PostMessageW(hwnd, WM_UPDATES, 0, 0);
    });
    let mut snapshot = None;
    let mut alerts = Alerts::default();
    let mut queue = VecDeque::new();
    let mut active: Option<(Notice, Instant)> = None;
    'messages: loop {
        let mut message = Message::default();
        match unsafe { GetMessageW(&mut message, window.0, 0, 0) } {
            -1 => return Err(io::Error::last_os_error()),
            0 => break,
            _ => {}
        }
        if !matches!(message.id, WM_UPDATES | WM_SHELL_WAKE) {
            unsafe { DispatchMessageW(&message) };
        }
        match watcher.take() {
            Some(Ok(update)) => {
                alerts.update(&update.snapshot, &mut queue);
                snapshot = Some(update.snapshot);
            }
            Some(Err(_)) => {
                icon.tip("Loupe · waiting for T3 records");
                snapshot = None;
            }
            None => {}
        }
        while let Some(event) = SHELL_EVENTS.with_borrow_mut(VecDeque::pop_front) {
            match event {
                ShellEvent::Close => break 'messages,
                ShellEvent::TaskbarCreated => {
                    icon.add()?;
                    active = None;
                }
                ShellEvent::Menu => match menu(&window, snapshot.as_ref(), &alerts) {
                    Some(MenuAction::Open) => open_dashboard(),
                    Some(MenuAction::Toggle(thread)) => alerts.toggle(thread),
                    Some(MenuAction::Exit) => break 'messages,
                    None => {}
                },
                ShellEvent::Open => open_dashboard(),
                ShellEvent::NoticeHidden => active = None,
                ShellEvent::NoticeClicked => {
                    if let Some((notice, _)) = active.take() {
                        let opened = integration::environment_id(path)
                            .and_then(|environment| {
                                integration::thread_link(&environment, &notice.thread_id)
                            })
                            .is_some_and(|link| integration::open_link(&link).is_ok());
                        if !opened {
                            open_dashboard();
                        }
                    }
                }
            }
        }
        if let Some(snapshot) = &snapshot {
            let running = local_threads(snapshot)
                .filter(|a| model::live(a) && !model::needs_you(a))
                .count();
            let waiting = alerts.waiting.as_ref().map_or(0, BTreeSet::len);
            icon.tip(&format!(
                "Loupe · {running} running · {waiting} waiting · {} watched",
                alerts.watched.len()
            ));
        }
        if active
            .as_ref()
            .is_some_and(|(_, sent)| sent.elapsed() > Duration::from_secs(45))
        {
            icon.dismiss();
            active = None;
        }
        if active.is_none()
            && let Some(notice) = queue.pop_front()
            && icon.notify(&notice)
        {
            active = Some((notice, Instant::now()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn shell_callbacks_reach_the_watcher_through_the_window_procedure() {
        let callback = |event| ((1 << 16) | event) as isize;
        unsafe {
            window_proc(0, WM_TRAY, 0, callback(WM_CONTEXTMENU));
            window_proc(0, WM_TRAY, 0, callback(NIN_BALLOONUSERCLICK));
        }
        let events = SHELL_EVENTS.take();
        assert!(matches!(events.front(), Some(ShellEvent::Menu)));
        assert!(matches!(events.get(1), Some(ShellEvent::NoticeClicked)));
        assert_eq!(events.len(), 2);
    }

    fn thread(id: &str, status: &str) -> Arc<Agent> {
        Arc::new(Agent {
            kind: "thread".into(),
            id: id.into(),
            thread_id: id.into(),
            title: format!("Task {id}"),
            status: status.into(),
            ..Agent::default()
        })
    }

    #[test]
    fn sends_new_attention_and_one_completion_for_each_watched_thread() {
        let mut alerts = Alerts::default();
        let mut queue = VecDeque::new();
        let mut snapshot = Snapshot {
            agents: vec![
                thread("a", "waiting"),
                thread("b", "running"),
                thread("c", "running"),
            ],
            ..Snapshot::default()
        };
        alerts.update(&snapshot, &mut queue);
        assert!(
            queue.is_empty(),
            "an existing backlog is only shown in the tooltip"
        );
        alerts.toggle("b".into());
        snapshot.agents = vec![
            thread("a", "waiting"),
            thread("b", "completed"),
            thread("c", "waiting"),
        ];
        alerts.update(&snapshot, &mut queue);
        alerts.update(&snapshot, &mut queue);
        assert_eq!(
            queue
                .iter()
                .map(|n| (n.thread_id.as_str(), n.title))
                .collect::<Vec<_>>(),
            vec![
                ("b", "Loupe · Watched thread finished"),
                ("c", "Loupe · Needs your input")
            ]
        );
        assert!(alerts.watched.is_empty());
    }
}
