#![windows_subsystem = "windows"]

mod api;
mod flyout;
mod http;
mod mcp;
mod preview;
mod providers;
mod render;
mod storage;
mod tokens;

use http::wide;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows_sys::core::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW, Shell_NotifyIconW, BIF_NEWDIALOGSTYLE,
    BIF_RETURNONLYFSDIRS, BROWSEINFOW, NIF_ICON, NIF_INFO, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_APP_RESULT: u32 = WM_APP + 1;
const WM_APP_CSWAP: u32 = WM_APP + 2; // claude-swap finished; lparam: Box<Result<(), String>>
const WM_APP_PROVIDER: u32 = WM_APP + 3;
const T_LAYOUT: usize = 1; // keeps the widget attached and positioned
const T_ROLL: usize = 5; // animation frames while numbers roll
/// How long a changed number rolls.
const ROLL_SECS: f32 = 0.30;
const T_TICK: usize = 3; // refreshes tooltip countdowns
const T_PROVIDER: usize = 4;
const T_SPARK_PULSE: usize = 6;
const T_FLYOUT: usize = 7;
const T_ALERT_REMOVE: usize = 8;
const T_FETCH_BASE: usize = 1000; // + account id, one-shot: that account's next fetch

const ID_REFRESH: usize = 1;
const ID_OPEN: usize = 2;
const ID_CSWAP: usize = 9;
const ID_SHOW_CLAUDE: usize = 24;
const ID_SHOW_CODEX: usize = 25;
const ID_SHOW_SPARK: usize = 26;
const ID_SPARK_ADDRESS: usize = 27;
const ID_ALERTS: usize = 28;
const ID_AUTOSTART: usize = 3;
const ID_EXIT: usize = 4;
const ID_SHOW_LEFT: usize = 5;
const ID_HIDE_ICON: usize = 6;
const ID_HIDE_LABELS: usize = 8;
const ID_ON_LEFT: usize = 11;
const ID_PIXEL_ICON: usize = 12;
const ID_RINGS: usize = 13;
const ID_RING_TEXT: usize = 14;
const ID_ROLL: usize = 15;
const ID_HIDE_FIVE: usize = 16;
const ID_HIDE_WEEK: usize = 17;
const ID_HIDE_FABLE: usize = 18;
const ID_CLAUDE_ICON: usize = 19;
const ID_RING_COLOR_ONLY: usize = 20;
const ID_RING_SPACE: usize = 21;
const ID_HIDE_PERCENT: usize = 22;
const ID_FAKE_DATA: usize = 23;
const ID_RING_MARGIN_BASE: usize = 600; // + index into RING_MARGINS
/// Gap between a ring's number and the ring (pixels at 100% scaling).
const RING_MARGINS: [u32; 7] = [0, 1, 2, 3, 4, 5, 6];
const DEFAULT_RING_MARGIN: u32 = 2;
/// Dev-only options exist in debug builds and with `--features dev`.
const DEV: bool = cfg!(any(debug_assertions, feature = "dev"));
/// Fake data changes this often.
const FAKE_DATA_SECS: u32 = 15;
const ID_ADD_ACCOUNT: usize = 7;
const ID_REMOVE_BASE: usize = 300; // + index into accounts
const ID_PER_COL_BASE: usize = 400; // + accounts per column (1 to 3)
const DEFAULT_PER_COL: u32 = 2;
const ID_FONT_BASE: usize = 200; // + index into FONT_SIZES
const ID_ICON_SIZE_BASE: usize = 500; // + index into ICON_SIZES
const ID_INTERVAL_BASE: usize = 100; // + index into INTERVALS

/// Update interval choices (seconds, menu label).
const INTERVALS: [(u32, &str); 7] = [
    (30, "30 seconds"),
    (60, "1 minute"),
    (120, "2 minutes"),
    (300, "5 minutes"),
    (600, "10 minutes"),
    (900, "15 minutes"),
    (1800, "30 minutes"),
];
const DEFAULT_INTERVAL: u32 = 120;

/// Font size choices (pixels at 100% scaling, menu label).
const FONT_SIZES: [(u32, &str); 6] = [
    (12, "Smallest"),
    (13, "Small"),
    (14, "Default"),
    (15, "Large"),
    (16, "Larger"),
    (18, "Largest"),
];
/// Icon size choices (pixels at 100% scaling, menu label).
const ICON_SIZES: [(u32, &str); 4] = [
    (14, "Small"),
    (18, "Default"),
    (22, "Large"),
    (26, "Largest"),
];
const DEFAULT_ICON_SIZE: u32 = 18;
use render::DEFAULT_FONT_SIZE;

const USAGE_URL: &str = "https://claude.ai/settings/usage";
const RUN_KEY: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: *const u16 = w!("TaskbarUsageWidget");
const SETTINGS_KEY: *const u16 = w!("Software\\TaskbarUsageWidget");

#[derive(Clone, PartialEq)]
struct RenderKey {
    h: i32,
    vertical: bool,
    dpi: u32,
    dark: bool,
    hover: bool,
    /// Per account: rounded 5-hour %, weekly %, Fable %, stale.
    rows: Vec<(Option<i64>, Option<i64>, Option<i64>, bool, Option<u32>, u8)>,
    show_left: bool,
    font_size: u32,
    icon_size: u32,
    hide_icon: bool,
    pixel_icon: bool,
    rings: bool,
    ring_text: bool,
    ring_color_only: bool,
    ring_space: bool,
    ring_margin: u32,
    hide_labels: bool,
    hide_percent: bool,
    hide_five: bool,
    hide_week: bool,
    hide_fable: bool,
    rows_per_col: u32,
    providers: (bool, bool, bool, Option<i64>, Option<i64>, u8, u8, u8),
}

/// One Claude Code login, found in its config folder.
struct Account {
    /// Stable id; also picks the account's fetch timer.
    id: u32,
    dir: PathBuf,
    /// Email, or the folder name until it's known.
    label: String,
    usage: Option<api::Usage>,
    /// While the numbers roll: the usage before the last update, and when it arrived.
    prev: Option<(api::Usage, Instant)>,
    last_ok: Option<i64>,
    error: Option<String>,
    fetching: bool,
    /// Rate-limit errors in a row; each one doubles the wait.
    rate_limited: u32,
    critical_active: [bool; 3],
    critical_since: [Option<Instant>; 3],
}

impl Account {
    fn new(id: u32, dir: PathBuf) -> Account {
        let label = api::account_label(&dir);
        Account {
            id,
            dir,
            label,
            usage: None,
            prev: None,
            last_ok: None,
            error: None,
            fetching: false,
            rate_limited: 0,
            critical_active: [false; 3],
            critical_since: [None; 3],
        }
    }

    fn is_stale(&self) -> bool {
        self.error.is_some() && self.last_ok.map_or(true, |t| now() - t > 600)
    }
}

struct State {
    ctl: HWND,
    widget: HWND,
    tip: HWND,
    taskbar: HWND,
    taskbar_created_msg: u32,
    accounts: Vec<Account>,
    next_id: u32,
    hover: bool,
    hover_since: Option<Instant>,
    leave_since: Option<Instant>,
    last_flyout_draw: Option<Instant>,
    dark: bool,
    rendered: Option<RenderKey>,
    pos: (i32, i32, i32, i32),
    /// Seconds between usage fetches.
    interval: u32,
    /// Show what's left instead of what's used.
    show_left: bool,
    /// Text size in pixels at 100% scaling.
    font_size: u32,
    icon_size: u32,
    /// Leave out the Claude mark.
    hide_icon: bool,
    /// Leave out the `5h` / `W` labels.
    hide_labels: bool,
    hide_percent: bool,
    /// Sit at the taskbar's left end instead of next to the tray.
    on_left: bool,
    /// Pixel-art mark instead of the asterisk.
    pixel_icon: bool,
    /// A progress ring for each limit, and whether its number sits inside it.
    rings: bool,
    ring_text: bool,
    ring_color_only: bool,
    ring_space: bool,
    ring_margin: u32,
    /// Roll the digits when a number changes.
    roll: bool,
    /// Limits turned off in "Show/hide limits". The 5-hour and Fable groups
    /// also hide on their own for plans without them.
    hide_five: bool,
    hide_week: bool,
    hide_fable: bool,
    /// Accounts stacked in each column of the widget.
    rows_per_col: u32,
    show_claude: bool,
    show_codex: bool,
    show_spark: bool,
    spark_host: String,
    codex: Option<providers::Reading>,
    codex_critical_active: [bool; 2],
    codex_critical_since: [Option<Instant>; 2],
    codex_sig: Vec<(PathBuf, u64, i64)>,
    codex_checked_at: i64,
    spark: Option<providers::SparkSnapshot>,
    spark_history: Vec<(i64, f64, f64)>,
    history: Option<storage::History>,
    token_tracker: std::sync::Arc<std::sync::Mutex<tokens::Tracker>>,
    token_totals: tokens::Totals,
    tokens_checked_at: i64,
    alerts: bool,
    alerted_periods: HashMap<String, i64>,
    high_windows: HashMap<String, (i64, bool)>,
    spark_was_up: Option<bool>,
    group_ranges: Vec<render::GroupRange>,
    /// Dev only: random usage instead of the API.
    fake_data: bool,
    /// Dev only: the tooltip shown while hovering a menu item.
    menu_tip: HWND,
    /// claude-swap's `cswap.exe`, found once at launch.
    cswap: Option<PathBuf>,
    /// A `cswap switch` is running.
    cswap_running: bool,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State {
        ctl: null_mut(), widget: null_mut(), tip: null_mut(), taskbar: null_mut(),
        taskbar_created_msg: 0, accounts: Vec::new(), next_id: 0,
        hover: false, hover_since: None, leave_since: None, last_flyout_draw: None,
        dark: false, rendered: None, pos: (0, 0, 0, 0),
        interval: DEFAULT_INTERVAL, show_left: false,
        font_size: DEFAULT_FONT_SIZE, icon_size: DEFAULT_ICON_SIZE, hide_icon: false, hide_labels: false, hide_percent: false, on_left: false, pixel_icon: false, rings: false, ring_text: false, ring_color_only: false, ring_space: false, ring_margin: DEFAULT_RING_MARGIN, roll: false, hide_five: false, hide_week: false, hide_fable: false, rows_per_col: DEFAULT_PER_COL,
        show_claude: true, show_codex: true, show_spark: true, spark_host: String::new(), codex: None,
        codex_critical_active: [false; 2], codex_critical_since: [None; 2],
        codex_sig: Vec::new(), codex_checked_at: 0, spark: None, spark_history: Vec::new(), history: None,
        token_tracker: std::sync::Arc::new(std::sync::Mutex::new(tokens::Tracker::default())),
        token_totals: tokens::Totals { claude: 0, claude_output: 0, codex: 0 }, tokens_checked_at: 0,
        alerts: true, alerted_periods: HashMap::new(), high_windows: HashMap::new(), spark_was_up: None,
        group_ranges: Vec::new(),
        fake_data: false, menu_tip: null_mut(),
        cswap: None, cswap_running: false,
    });
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn main() {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.iter().any(|argument| argument == "--mcp") {
        mcp::run();
        return;
    }
    if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--preview")
    {
        let Some(directory) = arguments.get(index + 1) else {
            println!("--preview requires an output directory");
            return;
        };
        match preview::run(Path::new(directory)) {
            Ok(paths) => {
                for path in paths {
                    println!("{}", path.display());
                }
            }
            Err(error) => println!("preview failed: {error}"),
        }
        return;
    }
    unsafe {
        let m = CreateMutexW(null(), 0, w!("Local\\TaskbarUsageWidget.Singleton"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(m);
            return;
        }
        // The folder picker needs COM on this thread.
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let hinst = GetModuleHandleW(null());
        let cursor = LoadCursorW(null_mut(), IDC_ARROW);
        flyout::register();
        for (name, proc_) in [
            (
                w!("TaskbarUsageCtl"),
                Some(ctl_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            ),
            (
                w!("TaskbarUsageWidget"),
                Some(
                    widget_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                ),
            ),
        ] {
            let wc = WNDCLASSW {
                lpfnWndProc: proc_,
                hInstance: hinst,
                lpszClassName: name,
                hCursor: cursor,
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        }
        let mut icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        InitCommonControlsEx(&mut icc);

        // Hidden top-level window: owns timers and receives broadcasts
        // (TaskbarCreated, theme changes) that a taskbar child would not.
        let ctl = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("TaskbarUsageCtl"),
            w!("Taskbar Usage Widget"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        with(|s| {
            s.ctl = ctl;
            s.taskbar_created_msg = msg;
            s.dark = system_dark();
            s.interval = read_setting("IntervalSeconds")
                .filter(|v| (30..=1800).contains(v))
                .unwrap_or(DEFAULT_INTERVAL);
            s.show_left = read_setting("ShowRemaining") == Some(1);
            s.icon_size = read_setting("IconSize")
                .filter(|v| ICON_SIZES.iter().any(|(px, _)| px == v))
                .unwrap_or(DEFAULT_ICON_SIZE);
            s.font_size = read_setting("FontSize")
                .filter(|v| FONT_SIZES.iter().any(|(f, _)| f == v))
                .unwrap_or(DEFAULT_FONT_SIZE);
            s.hide_icon = read_setting("HideIcon") == Some(1);
            s.hide_labels = read_setting("HideLabels") == Some(1);
            s.hide_percent = read_setting("HidePercent") == Some(1);
            s.on_left = read_setting("OnLeft") == Some(1);
            s.pixel_icon = read_setting("PixelIcon") == Some(1);
            s.rings = read_setting("Rings") == Some(1);
            s.ring_text = read_setting("RingText") == Some(1);
            s.ring_color_only = read_setting("RingColorOnly") == Some(1);
            s.ring_space = read_setting("RingSpace") == Some(1);
            s.ring_margin = read_setting("RingMargin")
                .filter(|m| RING_MARGINS.contains(m))
                .unwrap_or(DEFAULT_RING_MARGIN);
            s.roll = read_setting("RollNumbers") == Some(1);
            s.hide_five = read_setting("HideFiveHour") == Some(1);
            s.hide_week = read_setting("HideWeekly") == Some(1);
            s.hide_fable = read_setting("HideFable") == Some(1);
            s.fake_data = DEV && read_setting("FakeData") == Some(1);
            s.show_claude = read_setting("ShowClaude") != Some(0);
            s.show_codex = read_setting("ShowCodex") != Some(0);
            s.show_spark = read_setting("ShowSpark") != Some(0);
            s.alerts = read_setting("Alerts") != Some(0);
            s.spark_host = read_string_setting("SparkHost").unwrap_or_default();
            s.cswap = find_cswap();
            s.rows_per_col = read_setting("AccountsPerColumn")
                .filter(|v| (1..=render::MAX_ROWS_PER_COLUMN as u32).contains(v))
                .unwrap_or(DEFAULT_PER_COL);
            for dir in load_account_dirs() {
                let id = s.next_id;
                s.next_id += 1;
                s.accounts.push(Account::new(id, dir));
            }
        });
        ensure_widget();
        SetTimer(ctl, T_LAYOUT, 1000, None);
        SetTimer(ctl, T_TICK, 30_000, None);
        SetTimer(ctl, T_ROLL, 15, None);
        with(|s| s.history = storage::History::load(now()).ok());
        SetTimer(ctl, T_PROVIDER, provider_poll_ms(), None);
        start_fetch_all();
        start_provider_fetch();

        let mut m: MSG = std::mem::zeroed();
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}

// ---------- data ----------

fn start_fetch(id: u32) {
    let job = with(|s| {
        let a = s.accounts.iter_mut().find(|a| a.id == id)?;
        if a.fetching {
            return None;
        }
        a.fetching = true;
        Some((s.ctl as isize, a.dir.clone(), s.fake_data))
    });
    let Some((ctl, dir, fake)) = job else { return };
    unsafe { KillTimer(ctl as HWND, T_FETCH_BASE + id as usize) };
    std::thread::spawn(move || {
        let r = if fake {
            Ok(api::fake_usage())
        } else {
            api::fetch(&dir)
        };
        let label = api::account_label(&dir);
        let msg = Box::new(FetchResult { id, label, r });
        unsafe { PostMessageW(ctl as HWND, WM_APP_RESULT, 0, Box::into_raw(msg) as LPARAM) };
    });
}

fn start_fetch_all() {
    for id in with(|s| s.accounts.iter().map(|a| a.id).collect::<Vec<_>>()) {
        start_fetch(id);
    }
}

struct FetchResult {
    id: u32,
    label: String,
    r: Result<api::Usage, api::FetchError>,
}

fn on_result(res: FetchResult) {
    let next = with(|s| {
        let interval = s.interval;
        let roll = s.roll;
        // Ignore results for an account removed while it was fetching.
        let a = s.accounts.iter_mut().find(|a| a.id == res.id)?;
        a.fetching = false;
        a.label = res.label;
        let limited = matches!(res.r, Err(api::FetchError::RateLimited));
        match res.r {
            Ok(u) => {
                let limits = [&u.five_hour, &u.weekly, &u.fable];
                let durations = [18000, 604800, 604800];
                for index in 0..3 {
                    let critical = limits[index].as_ref().is_some_and(|limit| {
                        limit.percent > 95.0
                            || providers::pace(
                                limit.percent,
                                limit.resets_at,
                                Some(durations[index]),
                                now(),
                                None,
                            )
                            .projected
                                >= 100.0
                    });
                    if critical && !a.critical_active[index] {
                        a.critical_since[index] = Some(Instant::now());
                    } else if !critical {
                        a.critical_since[index] = None;
                    }
                    a.critical_active[index] = critical;
                }
                a.prev = a
                    .usage
                    .clone()
                    .filter(|_| roll)
                    .map(|old| (old, Instant::now()));
                a.usage = Some(u);
                a.last_ok = Some(now());
                a.error = None;
            }
            Err(e) => a.error = Some(e.message()),
        }
        if limited {
            a.rate_limited += 1;
        } else {
            a.rate_limited = 0;
        }
        let mut wait = if s.fake_data {
            FAKE_DATA_SECS
        } else {
            backoff_secs(interval, a.rate_limited)
        };
        if !s.fake_data && session_inactive() {
            wait = wait.max(900);
        }
        let next_reset = a
            .usage
            .iter()
            .flat_map(|usage| [&usage.five_hour, &usage.weekly, &usage.fable])
            .flatten()
            .filter_map(|limit| limit.resets_at)
            .filter(|reset| *reset > now())
            .min();
        if let Some(reset) = next_reset {
            let until = (reset - now()) as u32;
            if until <= wait {
                wait = until.saturating_add(30);
            }
        }
        Some((s.ctl, wait))
    });
    if let Some((ctl, secs)) = next {
        unsafe {
            SetTimer(ctl, T_FETCH_BASE + res.id as usize, secs * 1000, None);
            SetTimer(ctl, T_ROLL, 15, None); // stops itself once nothing rolls
        }
    }
    ensure_widget();
    update_tooltip();
    with(|s| persist_state(s, now()));
    for notice in with(evaluate_alerts) {
        show_alert(&notice);
    }
}

/// Wait before the next fetch. Normally the user's interval. After rate-limit
/// errors in a row it doubles each time, up to 10 minutes (never below the
/// user's interval).
fn backoff_secs(interval: u32, rate_limited: u32) -> u32 {
    const MAX_BACKOFF: u32 = 600;
    let grown = interval.saturating_mul(1 << rate_limited.min(10));
    grown.min(MAX_BACKOFF).max(interval)
}

// ---------- widget ----------

/// Creates the widget inside the taskbar if needed, positions it just left of
/// the tray area, and redraws it when anything visible changed.
fn ensure_widget() {
    unsafe {
        let taskbar = FindWindowW(w!("Shell_TrayWnd"), null());
        let (mut widget, old_taskbar) = with(|s| (s.widget, s.taskbar));
        if taskbar.is_null() {
            return;
        }
        if widget.is_null() || IsWindow(widget) == 0 || taskbar != old_taskbar {
            if !widget.is_null() && IsWindow(widget) != 0 {
                DestroyWindow(widget);
            }
            widget = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("TaskbarUsageWidget"),
                w!("Taskbar Usage Widget"),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                taskbar,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            if widget.is_null() {
                return;
            }
            with(|s| {
                s.widget = widget;
                s.taskbar = taskbar;
                s.tip = null_mut();
                s.rendered = None;
                s.pos = (0, 0, 0, 0);
            });
            update_tooltip();
        }

        let mut tb = std::mem::zeroed::<RECT>();
        GetWindowRect(taskbar, &mut tb);
        let tray = FindWindowExW(taskbar, null_mut(), w!("TrayNotifyWnd"), null());
        let mut tr = std::mem::zeroed::<RECT>();
        if tray.is_null() || GetWindowRect(tray, &mut tr) == 0 {
            tr = tb;
            tr.left = tb.right;
            tr.top = tb.bottom;
        }
        // A vertical taskbar (ExplorerPatcher, Windhawk, Windows 10) is taller than wide:
        // the widget then stacks its lines and `h` is the bar's width.
        let vertical = tb.bottom - tb.top > tb.right - tb.left;
        let h = if vertical {
            tb.right - tb.left
        } else {
            tb.bottom - tb.top
        };
        let dpi = GetDpiForWindow(taskbar).max(96);
        let scale = dpi as f32 / 96.0;

        let (key, needs_render, model) = with(|s| {
            let rows: Vec<render::Row> = s
                .accounts
                .iter()
                .map(|a| {
                    let u = a.usage.clone().unwrap_or_default();
                    render::Row {
                        five: u.five_hour.map(|l| l.percent),
                        week: u.weekly.map(|l| l.percent),
                        fable: u.fable.map(|l| l.percent),
                        roll: a.prev.as_ref().and_then(|(o, at)| {
                            let t = at.elapsed().as_secs_f32() / ROLL_SECS;
                            let pct = |l: Option<api::Limit>| l.map(|l| l.percent);
                            (t < 1.0)
                                .then_some(([pct(o.five_hour), pct(o.weekly), pct(o.fable)], t))
                        }),
                        stale: a.is_stale(),
                        pace: [
                            u.five_hour.and_then(|l| {
                                l.resets_at.map(|r| {
                                    providers::pace(l.percent, Some(r), Some(18000), now(), None)
                                        .elapsed_frac as f32
                                })
                            }),
                            u.weekly.and_then(|l| {
                                l.resets_at.map(|r| {
                                    providers::pace(l.percent, Some(r), Some(604800), now(), None)
                                        .elapsed_frac as f32
                                })
                            }),
                            u.fable.and_then(|l| {
                                l.resets_at.map(|r| {
                                    providers::pace(l.percent, Some(r), Some(604800), now(), None)
                                        .elapsed_frac as f32
                                })
                            }),
                        ],
                        projected: [
                            u.five_hour.map(|l| {
                                providers::pace(l.percent, l.resets_at, Some(18000), now(), None)
                                    .projected as f32
                            }),
                            u.weekly.map(|l| {
                                providers::pace(l.percent, l.resets_at, Some(604800), now(), None)
                                    .projected as f32
                            }),
                            u.fable.map(|l| {
                                providers::pace(l.percent, l.resets_at, Some(604800), now(), None)
                                    .projected as f32
                            }),
                        ],
                        text: [None, None, None],
                        critical: [false; 3],
                        loading: a.usage.is_none() && a.fetching,
                        pulse_alpha: if a.usage.is_none() && a.fetching {
                            loading_pulse_alpha()
                        } else {
                            a.critical_since
                                .iter()
                                .flatten()
                                .map(critical_pulse_alpha)
                                .fold(1.0, f32::min)
                        },
                    }
                })
                .collect();
            // Plans without a 5-hour limit (Ultra) report none: hide the group
            // on its own once every loaded account says so.
            let mut loaded = s
                .accounts
                .iter()
                .filter_map(|a| a.usage.as_ref())
                .peekable();
            let hide_five =
                s.hide_five || (loaded.peek().is_some() && loaded.all(|u| !u.has_five_hour));
            let key = RenderKey {
                h,
                vertical,
                dpi,
                dark: s.dark,
                hover: s.hover,
                rows: rows
                    .iter()
                    .map(|r| {
                        let pct = |v: Option<f64>| v.map(|p| p.round() as i64);
                        (
                            pct(r.five),
                            pct(r.week),
                            pct(r.fable),
                            r.stale,
                            r.roll.map(|(_, t)| (t * 1000.0) as u32),
                            (r.pulse_alpha * 100.0) as u8,
                        )
                    })
                    .collect(),
                show_left: s.show_left,
                font_size: s.font_size,
                icon_size: s.icon_size,
                hide_icon: s.hide_icon,
                pixel_icon: s.pixel_icon,
                rings: s.rings,
                ring_text: s.ring_text,
                ring_color_only: s.ring_color_only,
                ring_space: s.ring_space,
                ring_margin: s.ring_margin,
                hide_labels: s.hide_labels,
                hide_percent: s.hide_percent,
                hide_five,
                hide_week: s.hide_week,
                hide_fable: s.hide_fable,
                rows_per_col: s.rows_per_col,
                providers: (
                    s.show_claude,
                    s.show_codex,
                    s.show_spark,
                    s.codex
                        .as_ref()
                        .and_then(|reading| reading.windows.first())
                        .map(|window| window.percent.round() as i64),
                    s.spark.as_ref().map(|spark| spark.tok_s.round() as i64),
                    if s.spark.as_ref().is_some_and(|spark| spark.busy) {
                        (spark_pulse_alpha() * 100.0) as u8
                    } else {
                        0
                    },
                    (if s.codex_checked_at == 0 {
                        loading_pulse_alpha()
                    } else {
                        s.codex_critical_since
                            .iter()
                            .flatten()
                            .map(critical_pulse_alpha)
                            .fold(1.0, f32::min)
                    } * 100.0) as u8,
                    (if !s.spark_host.is_empty() && s.spark.is_none() {
                        loading_pulse_alpha()
                    } else {
                        1.0
                    } * 100.0) as u8,
                ),
            };
            let codex_rows = s
                .codex
                .as_ref()
                .map(|c| {
                    let find = |k: &str| c.windows.iter().find(|w| w.key == k);
                    let mk = |w: Option<&providers::Window>| w.map(|x| x.percent);
                    let pp = |w: Option<&providers::Window>| {
                        w.map(|x| {
                            providers::pace(x.percent, x.resets_at, x.window_secs, now(), None)
                        })
                    };
                    let f = find("5h");
                    let w = find("week");
                    let pf = pp(f);
                    let pw = pp(w);
                    vec![render::Row {
                        five: mk(f),
                        week: mk(w),
                        fable: None,
                        roll: None,
                        stale: c.error.is_some() || now() - c.as_of > 21600,
                        pace: [
                            pf.map(|p| p.elapsed_frac as f32),
                            pw.map(|p| p.elapsed_frac as f32),
                            None,
                        ],
                        projected: [
                            pf.map(|p| p.projected as f32),
                            pw.map(|p| p.projected as f32),
                            None,
                        ],
                        text: [None, None, None],
                        critical: [false; 3],
                        loading: false,
                        pulse_alpha: s
                            .codex_critical_since
                            .iter()
                            .flatten()
                            .map(critical_pulse_alpha)
                            .fold(1.0, f32::min),
                    }]
                })
                .unwrap_or_else(|| {
                    (s.codex_checked_at == 0)
                        .then(|| render::Row {
                            five: None,
                            week: None,
                            fable: None,
                            roll: None,
                            stale: false,
                            pace: [None; 3],
                            projected: [None; 3],
                            text: [None, None, None],
                            critical: [false; 3],
                            loading: true,
                            pulse_alpha: loading_pulse_alpha(),
                        })
                        .into_iter()
                        .collect()
                });
            let spark_row = s
                .spark
                .as_ref()
                .map(|spark| {
                    let down = spark.error.is_some() || !spark.up;
                    let text = if down {
                        "down".to_string()
                    } else if spark.tok_s <= 0.0 {
                        "idle".to_string()
                    } else {
                        format!("{:.0} t/s", spark.tok_s)
                    };
                    render::Row {
                        five: Some(spark.tok_s),
                        week: None,
                        fable: None,
                        roll: None,
                        stale: false,
                        pace: [None; 3],
                        projected: [None; 3],
                        text: [Some(text), None, None],
                        critical: [down, false, false],
                        loading: false,
                        pulse_alpha: 1.0,
                    }
                })
                .or_else(|| {
                    (!s.spark_host.is_empty()).then(|| render::Row {
                        five: None,
                        week: None,
                        fable: None,
                        roll: None,
                        stale: false,
                        pace: [None; 3],
                        projected: [None; 3],
                        text: [None, None, None],
                        critical: [false; 3],
                        loading: true,
                        pulse_alpha: loading_pulse_alpha(),
                    })
                });
            let model = render::Model {
                rows,
                hover: s.hover,
                dark: s.dark,
                show_left: s.show_left,
                font_size: s.font_size,
                icon_size: s.icon_size,
                hide_icon: s.hide_icon,
                pixel_icon: s.pixel_icon,
                rings: s.rings,
                ring_text: s.ring_text,
                ring_color_only: s.ring_color_only,
                ring_space: s.ring_space,
                ring_margin: s.ring_margin,
                hide_labels: s.hide_labels,
                hide_percent: s.hide_percent,
                hide_five,
                hide_week: s.hide_week,
                hide_fable: s.hide_fable,
                rows_per_col: s.rows_per_col as usize,
                provider_mark: 0,
                codex_rows,
                spark_row,
                show_claude: s.show_claude,
                show_codex: s.show_codex,
                show_spark: s.show_spark && !s.spark_host.is_empty(),
                spark_points: s.spark_history.iter().map(|x| x.1 as f32).collect(),
                spark_busy: s.spark.as_ref().is_some_and(|x| x.busy),
                spark_down: s.spark.as_ref().is_some_and(|x| x.error.is_some() || !x.up),
                spark_pulse_alpha: spark_pulse_alpha(),
            };
            let changed = s.rendered.as_ref() != Some(&key);
            (key, changed, model)
        });

        let (mut w, mut wh) = with(|s| (s.pos.2, s.pos.3));
        if needs_render {
            let frame = render::render(&model, h, scale, vertical.then_some(h));
            w = frame.w;
            wh = frame.h;
            present(widget, &frame);
            debug_assert!(frame.groups.iter().all(|range| {
                range.x0 < range.x1
                    && matches!(
                        range.group,
                        render::Group::Claude | render::Group::Codex | render::Group::Spark
                    )
            }));
            with(|s| {
                s.group_ranges = frame.groups.clone();
                s.rendered = Some(key);
            });
        }

        let gap = (2.0 * scale) as i32;
        let on_left = with(|s| s.on_left);
        let pos = if vertical {
            // Centered across the bar, above the tray, or at the top with "left side".
            let y = if on_left {
                gap
            } else {
                tr.top - tb.top - wh - gap
            };
            (((h - w) / 2).max(0), y.max(0), w, wh)
        } else {
            // The left end is free on a centered taskbar (unless the Widgets button is on).
            let x = if on_left {
                gap
            } else {
                tr.left - tb.left - w - gap
            };
            (x.max(0), 0, w, h)
        };
        let moved = with(|s| {
            let changed = s.pos != pos;
            s.pos = pos;
            changed
        });
        // Stay above the taskbar's own XAML content.
        let on_top = GetWindow(taskbar, GW_CHILD) == widget;
        if moved || !on_top || IsWindowVisible(widget) == 0 {
            SetWindowPos(
                widget,
                HWND_TOP,
                pos.0,
                pos.1,
                pos.2,
                pos.3,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
}

unsafe fn present(widget: HWND, f: &render::Frame) {
    let dc = CreateCompatibleDC(null_mut());
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: f.w,
            biHeight: -f.h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };
    let mut bits = null_mut();
    let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(f.px.as_ptr(), bits as *mut [u8; 4], f.px.len());
    let old = SelectObject(dc, bmp);
    let size = SIZE { cx: f.w, cy: f.h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(
        widget,
        null_mut(),
        null(),
        &size,
        dc,
        &src,
        0,
        &blend,
        ULW_ALPHA,
    );
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
}

fn tool_info(widget: HWND, text: *mut u16) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: widget,
        uId: widget as usize,
        lpszText: text,
        ..unsafe { std::mem::zeroed() }
    }
}

fn update_tooltip() {
    let (tip, widget, text) = with(|s| (s.tip, s.widget, tooltip_text(s)));
    if tip.is_null() {
        return;
    }
    let mut t = wide(&text);
    let ti = tool_info(widget, t.as_mut_ptr());
    unsafe { SendMessageW(tip, TTM_UPDATETIPTEXTW, 0, &ti as *const _ as LPARAM) };
}

fn tooltip_text(s: &State) -> String {
    let mut out = String::from("Taskbar Usage Widget");
    let multi = s.accounts.len() > 1;
    let indent = if multi { "  " } else { "" };
    let line = |name: &str, l: Option<api::Limit>, with_day: bool| -> String {
        match l {
            None => format!("\n{indent}{name}: –"),
            Some(l) => {
                let used = l.percent.round().clamp(0.0, 100.0) as i64;
                let mut t = format!("\n{indent}{name}: {used}% used, {}% left", 100 - used);
                if let Some(at) = l.resets_at {
                    t += &format!(
                        " · resets in {} ({})",
                        countdown(at - now()),
                        local_time(at, with_day)
                    );
                }
                t
            }
        }
    };
    for (i, a) in s.accounts.iter().enumerate() {
        if multi {
            // Say where the account sits: columns top to bottom, then left to right.
            let per = s.rows_per_col as usize;
            out += &format!("\n\n{}. {}", i + 1, a.label);
            if s.accounts.len() > per {
                out += &format!(" (column {}, row {})", i / per + 1, i % per + 1);
            }
        }
        let u = a.usage.clone().unwrap_or_default();
        if a.usage.is_some() {
            if u.has_five_hour {
                out += &line("5-hour", u.five_hour, false);
            }
            out += &line("Weekly", u.weekly, true);
            if u.fable.is_some() {
                out += &line("Fable (weekly)", u.fable, true);
            }
            for e in &u.extra {
                out += &line(&e.label, Some(e.limit), true);
            }
        }
        if let Some(t) = a.last_ok {
            out += &format!("\n{indent}Updated {}", local_time(t, false));
        }
        if let Some(e) = &a.error {
            out += &format!("\n{indent}⚠ {e}");
        } else if a.usage.is_none() {
            out += &format!("\n{indent}Loading…");
        }
    }
    if s.show_codex {
        out += "\n\nCodex";
        if let Some(c) = &s.codex {
            if let Some(p) = &c.plan_type {
                out += &format!(" ({p})");
            }
            for w in &c.windows {
                let p = providers::pace(w.percent, w.resets_at, w.window_secs, now(), None);
                out += &format!(
                    "\n{}: {:.0}% used - {}",
                    w.label,
                    w.percent,
                    providers::forecast_wording(p, w.resets_at)
                );
                if let Some(r) = w.resets_at {
                    out += &format!(" - Resets in {}", providers::resets_in(r - now()));
                }
                if w.reset_since_last_run {
                    out += " (reset since last Codex run)";
                }
            }
            if now() - c.as_of > 21600 {
                out += "\n(stale)";
            }
            if let Some(e) = &c.error {
                out += &format!("\n{e}");
            }
        } else {
            out += "\nLoading...";
        }
    }
    if s.show_spark && !s.spark_host.is_empty() {
        out += "\n\nSpark";
        if let Some(sp) = &s.spark {
            if let Some(e) = &sp.error {
                out += &format!("\nDown: {e}");
            } else {
                out += &format!(
                    "\n{} - {:.0} t/s - GPU {:.0}%",
                    if sp.busy { "Busy" } else { "Idle" },
                    sp.tok_s,
                    sp.gpu_util
                );
            }
        } else {
            out += "\nLoading...";
        }
    }
    out
}

fn json_is_active(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or(false)
        || value.as_i64().is_some_and(|count| count > 0)
        || value.as_u64().is_some_and(|count| count > 0)
}

fn flyout_snapshot(s: &State) -> flyout::Snapshot {
    let t = now();
    let mut sections = Vec::new();
    if s.show_claude {
        for (index, reading) in claude_readings(s, t).into_iter().enumerate() {
            let mut section = flyout::Section {
                provider: flyout::ProviderKind::Claude,
                title: "Claude".into(),
                chip: if s.accounts.len() > 1 {
                    format!("account {}", index + 1)
                } else {
                    String::new()
                },
                stale: reading.error.is_some() || t - reading.as_of > 600,
                error: reading.error.clone(),
                ..Default::default()
            };
            add_flyout_windows(&mut section, &reading, t);
            section.today = if s.tokens_checked_at == 0 {
                "Today: counting...".into()
            } else {
                format!(
                    "Today: {} tokens · {} out",
                    compact_number(s.token_totals.claude),
                    compact_number(s.token_totals.claude_output)
                )
            };
            if let Some(history) = &s.history {
                section.chart = history
                    .samples("claude", "5h")
                    .into_iter()
                    .filter(|sample| sample.0 >= t - 86400)
                    .map(|sample| sample.1)
                    .collect();
            }
            sections.push(section);
        }
    }
    if s.show_codex {
        if let Some(reading) = &s.codex {
            let mut section = flyout::Section {
                provider: flyout::ProviderKind::Codex,
                title: "Codex".into(),
                chip: reading
                    .plan_type
                    .as_deref()
                    .map(title_case)
                    .unwrap_or_default(),
                stale: reading.error.is_some() || t - reading.as_of > 21600,
                error: reading.error.clone(),
                ..Default::default()
            };
            add_flyout_windows(&mut section, reading, t);
            section.today = if s.tokens_checked_at == 0 {
                "Today: counting...".into()
            } else {
                format!("Today: {} tokens", compact_number(s.token_totals.codex))
            };
            section.as_of = format!("as of {} ago", human_short(t - reading.as_of));
            sections.push(section);
        }
    }
    if s.show_spark {
        let mut section = flyout::Section {
            provider: flyout::ProviderKind::Spark,
            title: "Spark".into(),
            ..Default::default()
        };
        if let Some(spark) = &s.spark {
            let endpoint = spark.endpoints.first();
            section.chip = endpoint
                .and_then(|value| value["model"].as_str())
                .unwrap_or_default()
                .to_owned();
            let running: i64 = spark
                .endpoints
                .iter()
                .filter_map(|value| value["running"].as_i64())
                .sum();
            let waiting: i64 = spark
                .endpoints
                .iter()
                .filter_map(|value| value["waiting"].as_i64())
                .sum();
            let peak = s
                .spark_history
                .iter()
                .map(|sample| sample.1)
                .fold(0.0f64, f64::max);
            section.spark = Some(flyout::SparkStats {
                tok_s: spark.tok_s,
                peak,
                running,
                waiting,
                kv_pct: endpoint.and_then(|value| value["kv_pct"].as_f64()),
                ttft_ms: endpoint
                    .and_then(|value| value["ttft_ms"].as_f64().or_else(|| value["ttft"].as_f64())),
                gpu_util: spark.gpu_util,
                gpu_temp: spark.sys["gpu"]["temp"].as_f64(),
                gpu_power: spark.sys["gpu"]["power"].as_f64(),
                mem_pct: spark.host["mem_pct"]
                    .as_f64()
                    .or_else(|| spark.sys["memory"]["pct"].as_f64()),
                swap_pct: spark.host["swap_pct"]
                    .as_f64()
                    .or_else(|| spark.sys["swap"]["pct"].as_f64()),
                aux: spark
                    .aux
                    .iter()
                    .map(|service| flyout::AuxStatus {
                        name: service["name"]
                            .as_str()
                            .unwrap_or("service")
                            .split_whitespace()
                            .next()
                            .unwrap_or("service")
                            .to_owned(),
                        up: service["up"].as_bool().unwrap_or(false),
                        busy: json_is_active(&service["running"])
                            || json_is_active(&service["pending"])
                            || service["busy"].as_bool().unwrap_or(false),
                    })
                    .collect(),
            });
            section.error = spark.error.clone();
            section.stale = t - spark.fetched_at > 30;
            section.chart = s.spark_history.iter().map(|sample| sample.1).collect();
        } else {
            section.error = Some("Not configured".into());
        }
        sections.push(section);
    }
    let newest = s
        .accounts
        .iter()
        .filter_map(|account| account.last_ok)
        .chain(s.codex.iter().map(|reading| reading.fetched_at))
        .chain(s.spark.iter().map(|spark| spark.fetched_at))
        .max()
        .unwrap_or(t);
    flyout::Snapshot {
        sections,
        updated: format!("updated {} ago", human_short(t - newest)),
    }
}

fn add_flyout_windows(section: &mut flyout::Section, reading: &providers::Reading, t: i64) {
    for window in &reading.windows {
        let pace = providers::pace(
            window.percent,
            window.resets_at,
            window.window_secs,
            t,
            None,
        );
        let reset = window
            .resets_at
            .map(|at| local_time(at, !same_local_day(at, t)))
            .unwrap_or_default();
        section.bars.push(flyout::Bar {
            label: window.label.clone(),
            percent: window.percent,
            elapsed: pace.elapsed_pct,
            reset,
            ahead_by: pace.ahead_by,
            projected: pace.projected,
            full_at: pace
                .full_at
                .map(|at| local_time(at, !same_local_day(at, t))),
        });
    }
}

fn compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn human_short(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 3600 {
        format!("{} h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{} m", seconds / 60)
    } else {
        format!("{} s", seconds)
    }
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn same_local_day(left: i64, right: i64) -> bool {
    let date = |unix: i64| unsafe {
        let ticks = (unix + 11_644_473_600) as u64 * 10_000_000;
        let file = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = std::mem::zeroed();
        let mut local = std::mem::zeroed();
        FileTimeToSystemTime(&file, &mut utc);
        SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local);
        (local.wYear, local.wMonth, local.wDay)
    };
    date(left) == date(right)
}

unsafe fn update_flyout() {
    show_flyout(false);
}

unsafe fn show_flyout(click: bool) {
    let (widget, dark, snapshot) = with(|state| (state.widget, state.dark, flyout_snapshot(state)));
    if widget.is_null() {
        return;
    }
    let mut anchor: RECT = std::mem::zeroed();
    GetWindowRect(widget, &mut anchor);
    let ctl = with(|state| state.ctl);
    if click {
        flyout::show_click(&snapshot, anchor, dark, ctl);
    } else {
        flyout::show(&snapshot, anchor, dark, ctl);
    }
    with(|state| state.last_flyout_draw = Some(Instant::now()));
}

fn countdown(secs: i64) -> String {
    let m = (secs.max(0) + 59) / 60;
    let (d, h, mm) = (m / 1440, (m % 1440) / 60, m % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {mm}m")
    } else {
        format!("{mm}m")
    }
}

pub(crate) fn local_time(unix: i64, with_day: bool) -> String {
    unsafe {
        let ft_val = (unix + 11_644_473_600) as u64 * 10_000_000;
        let ft = FILETIME {
            dwLowDateTime: ft_val as u32,
            dwHighDateTime: (ft_val >> 32) as u32,
        };
        let mut utc = std::mem::zeroed();
        let mut local = std::mem::zeroed();
        FileTimeToSystemTime(&ft, &mut utc);
        SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local);
        let mut buf = [0u16; 64];
        let mut out = String::new();
        if with_day {
            let n = GetDateFormatEx(null(), 0, &local, w!("ddd"), buf.as_mut_ptr(), 64, null());
            if n > 1 {
                out = String::from_utf16_lossy(&buf[..n as usize - 1]) + " ";
            }
        }
        let n = GetTimeFormatEx(null(), TIME_NOSECONDS, &local, null(), buf.as_mut_ptr(), 64);
        if n > 1 {
            out += &String::from_utf16_lossy(&buf[..n as usize - 1]);
        }
        out
    }
}

// ---------- settings ----------

fn system_dark() -> bool {
    let mut v: u32 = 1;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut u32 as *mut _,
            &mut size,
        )
    };
    r == ERROR_SUCCESS && v == 0
}

fn read_setting(name: &str) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            wide(name).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut u32 as *mut _,
            &mut size,
        )
    };
    (r == ERROR_SUCCESS).then_some(v)
}

fn write_setting(name: &str, v: u32) {
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            wide(name).as_ptr(),
            REG_DWORD,
            &v as *const u32 as *const _,
            4,
        );
    }
}

fn set_interval(secs: u32) {
    with(|s| s.interval = secs);
    write_setting("IntervalSeconds", secs);
    // Apply right away: fetch now, then keep the new pace. Accounts that are
    // mid-fetch pick up the new pace when they finish.
    start_fetch_all();
}

fn read_string_setting(name: &str) -> Option<String> {
    let name = wide(name);
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        size = (buf.len() * 2) as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut _,
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn write_string_setting(name: &str, v: &str) {
    let v = wide(v);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            SETTINGS_KEY,
            wide(name).as_ptr(),
            REG_SZ,
            v.as_ptr() as *const _,
            (v.len() * 2) as u32,
        );
    }
}

// ---------- accounts ----------

/// Saved account folders, separated by `|` (which paths can't contain).
/// With nothing saved, the one account Claude Code uses by default.
fn load_account_dirs() -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = read_string_setting("Accounts")
        .unwrap_or_default()
        .split('|')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect();
    if dirs.is_empty() {
        vec![api::default_dir()]
    } else {
        dirs
    }
}

fn save_accounts() {
    let joined = with(|s| {
        s.accounts
            .iter()
            .map(|a| a.dir.display().to_string())
            .collect::<Vec<_>>()
            .join("|")
    });
    write_string_setting("Accounts", &joined);
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}

unsafe fn message(text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let ctl = with(|s| s.ctl);
    MessageBoxW(
        ctl,
        wide(text).as_ptr(),
        w!("Claude Usage Widget"),
        flags | MB_SETFOREGROUND,
    )
}

/// Asks for a folder with the standard Windows folder picker.
unsafe fn pick_folder() -> Option<PathBuf> {
    let ctl = with(|s| s.ctl);
    let title = wide(
        "Select the account's Claude Code folder (the one CLAUDE_CONFIG_DIR points to, \
         e.g. .claude-work), or the user folder that contains .claude.",
    );
    let bi = BROWSEINFOW {
        hwndOwner: ctl,
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..std::mem::zeroed()
    };
    let pidl = SHBrowseForFolderW(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr()) != 0;
    CoTaskMemFree(pidl as *const _);
    if !ok {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

unsafe fn add_account() {
    let Some(picked) = pick_folder() else { return };
    // Accept the config folder itself, or a home folder holding `.claude`.
    let dir = if api::has_login(&picked) {
        picked
    } else if api::has_login(&picked.join(".claude")) {
        picked.join(".claude")
    } else {
        let text = format!(
            "No Claude Code login found in\n{}\n\nAdd it anyway? It will show up once you sign in \
             to Claude Code with this folder.",
            picked.display()
        );
        if message(&text, MB_YESNO | MB_ICONWARNING) != IDYES {
            return;
        }
        picked
    };
    if with(|s| s.accounts.iter().any(|a| same_dir(&a.dir, &dir))) {
        message(
            "This account is already in the widget.",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    let id = with(|s| {
        let id = s.next_id;
        s.next_id += 1;
        s.accounts.push(Account::new(id, dir));
        id
    });
    save_accounts();
    ensure_widget();
    update_tooltip();
    start_fetch(id);
}

unsafe fn remove_account(index: usize) {
    let Some(label) = with(|s| s.accounts.get(index).map(|a| a.label.clone())) else {
        return;
    };
    let text =
        format!("Remove {label} from the widget?\n\nThis doesn't sign it out of Claude Code.");
    if message(&text, MB_YESNO | MB_ICONQUESTION) != IDYES {
        return;
    }
    let (ctl, id) = with(|s| {
        let a = s.accounts.remove(index);
        (s.ctl, a.id)
    });
    KillTimer(ctl, T_FETCH_BASE + id as usize);
    save_accounts();
    ensure_widget();
    update_tooltip();
}

fn autostart_enabled() -> bool {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
    }
}

fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v = wide(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ,
                v.as_ptr() as *const _,
                (v.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

// ---------- claude-swap ----------

/// Finds claude-swap's `cswap.exe`: on PATH, or in `~\.localin`, where
/// `uv tool install` and `pipx` put it.
fn find_cswap() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Ok(home) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(home).join(".local").join("bin"));
    }
    dirs.into_iter()
        .map(|d| d.join("cswap.exe"))
        .find(|p| p.is_file())
}

/// Runs `cswap switch` (rotate to the next account) in the background, then
/// refreshes the widget. JSON mode never stops to ask a question.
fn run_cswap_switch(exe: PathBuf) {
    let ctl = with(|s| {
        if s.cswap_running {
            return None;
        }
        s.cswap_running = true;
        Some(s.ctl as isize)
    });
    let Some(ctl) = ctl else { return };
    std::thread::spawn(move || {
        let r = Box::new(cswap_switch(&exe));
        unsafe { PostMessageW(ctl as HWND, WM_APP_CSWAP, 0, Box::into_raw(r) as LPARAM) };
    });
}

fn cswap_switch(exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const TIMEOUT_SECS: u64 = 60;

    let mut child = Command::new(exe)
        .args(["switch", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Could not start cswap: {e}"))?;
    // Read the pipes on their own threads so a chatty cswap can't block on a full pipe.
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as _));
    let err = read(child.stderr.take().map(|p| Box::new(p) as _));
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed().as_secs() < TIMEOUT_SECS => {
                std::thread::sleep(std::time::Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                return Err(format!(
                    "cswap didn't finish within {TIMEOUT_SECS} seconds."
                ));
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let json: Option<serde_json::Value> = serde_json::from_str(stdout.trim()).ok();
    if let Some(v) = &json {
        if let Some(m) = v["error"]["message"].as_str() {
            return Err(m.to_string());
        }
        if v["switched"] == serde_json::Value::Bool(false) {
            let reason = v["reason"]
                .as_str()
                .unwrap_or("cswap didn't switch accounts.");
            return Err(reason.to_string());
        }
    }
    if status.success() {
        Ok(())
    } else {
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|t| !t.is_empty());
        Err(detail
            .unwrap_or("cswap failed.")
            .chars()
            .take(500)
            .collect())
    }
}

struct ProviderResult {
    codex: providers::Reading,
    codex_sig: Vec<(PathBuf, u64, i64)>,
    codex_checked: bool,
    spark: Option<providers::SparkSnapshot>,
    tokens: Option<tokens::Totals>,
}

fn start_provider_fetch() {
    let (ctl, host, fake, old_codex, old_sig, due, tracker, dirs, tokens_due) = with(|s| {
        (
            s.ctl as isize,
            s.spark_host.clone(),
            s.fake_data,
            s.codex.clone(),
            s.codex_sig.clone(),
            now() - s.codex_checked_at >= 60,
            s.token_tracker.clone(),
            s.accounts
                .iter()
                .map(|account| account.dir.clone())
                .collect::<Vec<_>>(),
            now() - s.tokens_checked_at >= 60,
        )
    });
    std::thread::spawn(move || {
        let t = now();
        let signature = providers::codex_files_signature();
        let codex = if !due {
            old_codex.unwrap_or_else(|| providers::read_codex(t))
        } else if !fake && signature == old_sig && old_codex.is_some() {
            old_codex.unwrap()
        } else if fake {
            providers::Reading {
                provider: providers::Provider::Codex,
                account: None,
                windows: vec![
                    providers::Window {
                        key: "week".into(),
                        label: "Weekly".into(),
                        percent: 37.0,
                        resets_at: Some(t + 4 * 86400),
                        window_secs: Some(604800),
                        reset_since_last_run: false,
                    },
                    providers::Window {
                        key: "5h".into(),
                        label: "5-hour".into(),
                        percent: 18.0,
                        resets_at: Some(t + 7200),
                        window_secs: Some(18000),
                        reset_since_last_run: false,
                    },
                ],
                fetched_at: t,
                as_of: t,
                error: None,
                plan_type: Some("dev".into()),
            }
        } else {
            providers::read_codex(t)
        };
        let spark = if host.is_empty() {
            None
        } else if fake {
            Some(providers::parse_spark(
                &serde_json::json!({"endpoints":[{"name":"dev","model":"local","engine":"dev","up":true,"tok_s":142.0,"running":1}],"sys":{"gpu":{"util":72.0}}}),
                t,
            ))
        } else {
            Some(providers::fetch_spark(&host, t))
        };
        let tokens = tokens_due
            .then(|| {
                tracker
                    .lock()
                    .ok()
                    .map(|mut tracker| tracker.poll(&dirs, t))
            })
            .flatten();
        let msg = Box::new(ProviderResult {
            codex,
            codex_sig: signature,
            codex_checked: due,
            spark,
            tokens,
        });
        unsafe {
            PostMessageW(
                ctl as HWND,
                WM_APP_PROVIDER,
                0,
                Box::into_raw(msg) as LPARAM,
            );
        }
    });
}

fn claude_readings(s: &State, t: i64) -> Vec<providers::Reading> {
    s.accounts
        .iter()
        .map(|a| {
            let mut windows = Vec::new();
            if let Some(u) = &a.usage {
                if let Some(l) = u.five_hour {
                    windows.push(providers::Window {
                        key: "5h".into(),
                        label: "5-hour".into(),
                        percent: l.percent,
                        resets_at: l.resets_at,
                        window_secs: Some(18000),
                        reset_since_last_run: false,
                    });
                }
                if let Some(l) = u.weekly {
                    windows.push(providers::Window {
                        key: "week".into(),
                        label: "Weekly".into(),
                        percent: l.percent,
                        resets_at: l.resets_at,
                        window_secs: Some(604800),
                        reset_since_last_run: false,
                    });
                }
                for e in &u.extra {
                    windows.push(providers::Window {
                        key: e.key.clone(),
                        label: e.label.clone(),
                        percent: e.limit.percent,
                        resets_at: e.limit.resets_at,
                        window_secs: Some(e.window_secs),
                        reset_since_last_run: false,
                    });
                }
            }
            providers::Reading {
                provider: providers::Provider::Claude,
                account: Some(a.label.clone()),
                windows,
                fetched_at: a.last_ok.unwrap_or(t),
                as_of: a.last_ok.unwrap_or(t),
                error: a.error.clone(),
                plan_type: None,
            }
        })
        .collect()
}

fn persist_state(s: &mut State, t: i64) {
    let mut readings = claude_readings(s, t);
    if let Some(c) = &s.codex {
        readings.push(c.clone());
    }
    if let Some(history) = &mut s.history {
        let _ = history.append(&readings, s.spark.as_ref(), t);
        let _ = storage::write_state(
            &readings,
            s.spark.as_ref(),
            history,
            t,
            (
                s.token_totals.claude,
                s.token_totals.claude_output,
                s.token_totals.codex,
            ),
            s.tokens_checked_at,
        );
    }
}

fn on_provider_result(r: ProviderResult) {
    let spark_notice = with(|s| {
        for (index, key) in ["5h", "week"].iter().enumerate() {
            let critical = r
                .codex
                .windows
                .iter()
                .find(|window| window.key == *key)
                .is_some_and(|window| {
                    window.percent > 95.0
                        || providers::pace(
                            window.percent,
                            window.resets_at,
                            window.window_secs,
                            now(),
                            None,
                        )
                        .projected
                            >= 100.0
                });
            if critical && !s.codex_critical_active[index] {
                s.codex_critical_since[index] = Some(Instant::now());
            } else if !critical {
                s.codex_critical_since[index] = None;
            }
            s.codex_critical_active[index] = critical;
        }
        s.codex = Some(r.codex);
        s.codex_sig = r.codex_sig;
        if r.codex_checked {
            s.codex_checked_at = now();
        }
        s.spark = r.spark;
        if let Some(tokens) = r.tokens {
            s.token_totals = tokens;
            s.tokens_checked_at = now();
        }
        if let Some(sp) = &s.spark {
            s.spark_history.push((sp.fetched_at, sp.tok_s, sp.gpu_util));
        }
        let cutoff = now() - 600;
        s.spark_history.retain(|x| x.0 >= cutoff);
        persist_state(s, now());
        s.rendered = None;
        spark_alert(s)
    });
    if let Some(notice) = spark_notice {
        show_alert(&notice);
    }
    unsafe {
        let (ctl, busy) = with(|s| (s.ctl, s.spark.as_ref().is_some_and(|spark| spark.busy)));
        if busy {
            SetTimer(ctl, T_SPARK_PULSE, 100, None);
        } else {
            KillTimer(ctl, T_SPARK_PULSE);
        }
        if with(|s| s.codex_critical_since.iter().any(Option::is_some)) {
            SetTimer(ctl, T_ROLL, 15, None);
        }
    }
    ensure_widget();
    update_tooltip();
}

fn spark_pulse_alpha() -> f32 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f32();
    0.65 + 0.35 * (seconds * std::f32::consts::TAU).cos()
}

fn loading_pulse_alpha() -> f32 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f32();
    0.7 + 0.3 * (seconds / 1.2 * std::f32::consts::TAU).cos()
}

fn critical_pulse_alpha(started: &Instant) -> f32 {
    let elapsed = started.elapsed().as_secs_f32();
    if elapsed >= 1.8 {
        1.0
    } else {
        0.675 + 0.325 * (elapsed / 0.6 * std::f32::consts::TAU).cos()
    }
}

fn provider_poll_ms() -> u32 {
    if session_inactive() {
        30_000
    } else {
        5_000
    }
}

fn session_inactive() -> bool {
    unsafe {
        let desktop = windows_sys::Win32::System::StationsAndDesktops::OpenInputDesktop(
            0,
            0,
            windows_sys::Win32::System::StationsAndDesktops::DESKTOP_SWITCHDESKTOP,
        );
        let locked = desktop.is_null()
            || windows_sys::Win32::System::StationsAndDesktops::SwitchDesktop(desktop) == 0;
        if !desktop.is_null() {
            windows_sys::Win32::System::StationsAndDesktops::CloseDesktop(desktop);
        }
        let mut input = windows_sys::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO {
            cbSize: std::mem::size_of::<
                windows_sys::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO,
            >() as u32,
            dwTime: 0,
        };
        let idle = windows_sys::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo(&mut input)
            != 0
            && windows_sys::Win32::System::SystemInformation::GetTickCount()
                .wrapping_sub(input.dwTime)
                >= 10 * 60 * 1000;
        locked || idle
    }
}

fn evaluate_alerts(s: &mut State) -> Vec<String> {
    if !s.alerts {
        return Vec::new();
    }
    let t = now();
    let mut notices = Vec::new();
    for reading in claude_readings(s, t)
        .into_iter()
        .chain(s.codex.iter().cloned())
    {
        let provider = match reading.provider {
            providers::Provider::Claude => "Claude",
            providers::Provider::Codex => "Codex",
            providers::Provider::Spark => "Spark",
        };
        for window in reading.windows {
            let reset = window.resets_at.unwrap_or(0);
            let key = format!("{provider}:{}", window.key);
            let pace = providers::pace(
                window.percent,
                window.resets_at,
                window.window_secs,
                t,
                None,
            );
            let high = window.percent >= 90.0
                && pace
                    .full_at
                    .zip(window.resets_at)
                    .is_some_and(|(full, end)| full < end);
            let previous = s.high_windows.insert(key.clone(), (reset, high));
            if high && s.alerted_periods.get(&key).copied() != Some(reset) {
                notices.push(format!(
                    "{provider} {} is at {:.0}% and may fill before reset.",
                    window.label, window.percent
                ));
                s.alerted_periods.insert(key.clone(), reset);
            }
            if previous.is_some_and(|(old_reset, was_high)| was_high && old_reset != reset) {
                notices.push(format!("{provider} {} has refilled.", window.label));
            }
        }
    }
    notices
}

fn spark_alert(s: &mut State) -> Option<String> {
    if !s.alerts {
        return None;
    }
    let up = s
        .spark
        .as_ref()
        .is_some_and(|spark| spark.up && spark.error.is_none());
    let old = s.spark_was_up.replace(up);
    match (old, up) {
        (Some(true), false) => Some("Spark is unreachable.".into()),
        (Some(false), true) => Some("Spark is back up.".into()),
        _ => None,
    }
}

fn show_alert(message_text: &str) {
    unsafe {
        let ctl = with(|s| s.ctl);
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = ctl;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_INFO;
        data.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
        data.dwInfoFlags = NIIF_INFO;
        copy_wide(&mut data.szInfoTitle, "Usage widget");
        copy_wide(&mut data.szInfo, message_text);
        Shell_NotifyIconW(NIM_ADD, &data);
        SetTimer(ctl, T_ALERT_REMOVE, 10_000, None);
    }
}

fn copy_wide<const N: usize>(destination: &mut [u16; N], text: &str) {
    let source = wide(text);
    let length = source.len().min(N).saturating_sub(1);
    destination[..length].copy_from_slice(&source[..length]);
}

unsafe fn remove_alert() {
    let mut data: NOTIFYICONDATAW = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = with(|s| s.ctl);
    data.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &data);
}

unsafe fn quit() {
    let (widget, ctl) = with(|s| (s.widget, s.ctl));
    if !widget.is_null() {
        DestroyWindow(widget);
    }
    DestroyWindow(ctl);
}

fn on_cswap_done(r: Result<(), String>) {
    with(|s| s.cswap_running = false);
    match r {
        // The live login changed: show the new account's numbers now.
        Ok(()) => start_fetch_all(),
        Err(e) => unsafe {
            message(
                &format!(
                    "Switching accounts with claude-swap failed:

{e}"
                ),
                MB_OK | MB_ICONWARNING,
            );
        },
    }
}

fn open_usage_page() {
    open_url(USAGE_URL);
}

fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(
            null_mut(),
            w!("open"),
            wide(url).as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

thread_local! { static SPARK_DIALOG: RefCell<(String,Option<String>)> = const { RefCell::new((String::new(),None)) }; }

unsafe extern "system" fn spark_dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, _: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            let text = SPARK_DIALOG.with(|x| x.borrow().0.clone());
            SetDlgItemTextW(hwnd, 100, wide(&text).as_ptr());
            1
        }
        WM_COMMAND if (wp & 0xffff) as usize == IDOK as usize => {
            let mut b = [0u16; 512];
            let n = GetDlgItemTextW(hwnd, 100, b.as_mut_ptr(), b.len() as i32);
            let s = String::from_utf16_lossy(&b[..n as usize])
                .trim()
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string();
            SPARK_DIALOG.with(|x| x.borrow_mut().1 = Some(s));
            EndDialog(hwnd, IDOK as isize);
            1
        }
        WM_COMMAND if (wp & 0xffff) as usize == IDCANCEL as usize => {
            EndDialog(hwnd, IDCANCEL as isize);
            1
        }
        WM_CLOSE => {
            EndDialog(hwnd, IDCANCEL as isize);
            1
        }
        _ => 0,
    }
}

fn prompt_spark_address() -> Option<String> {
    let current = with(|s| s.spark_host.clone());
    SPARK_DIALOG.with(|x| *x.borrow_mut() = (current, None));
    let mut v = Vec::<u16>::new();
    let d = |v: &mut Vec<u16>, x: u32| {
        v.push(x as u16);
        v.push((x >> 16) as u16)
    };
    let z = |v: &mut Vec<u16>, s: &str| {
        v.extend(s.encode_utf16());
        v.push(0)
    };
    d(
        &mut v,
        (WS_POPUP | WS_CAPTION | WS_SYSMENU) as u32 | DS_MODALFRAME as u32,
    );
    d(&mut v, 0);
    v.extend([4, 10, 10, 210, 70]);
    v.extend([0, 0]);
    z(&mut v, "Spark address");
    let item = |v: &mut Vec<u16>,
                style: u32,
                x: u16,
                y: u16,
                cx: u16,
                cy: u16,
                id: u16,
                class: u16,
                title: &str| {
        if v.len() % 2 != 0 {
            v.push(0)
        }
        d(v, style);
        d(v, 0);
        v.extend([x, y, cx, cy, id, 0xffff, class]);
        z(v, title);
        v.push(0)
    };
    item(
        &mut v,
        (WS_CHILD | WS_VISIBLE) as u32,
        8,
        8,
        194,
        10,
        101,
        0x0082,
        "Host or host:port",
    );
    item(
        &mut v,
        (WS_CHILD | WS_VISIBLE | WS_BORDER | WS_TABSTOP) as u32 | ES_AUTOHSCROLL as u32,
        8,
        21,
        194,
        14,
        100,
        0x0081,
        "",
    );
    item(
        &mut v,
        (WS_CHILD | WS_VISIBLE | WS_TABSTOP) as u32 | BS_DEFPUSHBUTTON as u32,
        92,
        45,
        52,
        14,
        IDOK as u16,
        0x0080,
        "OK",
    );
    item(
        &mut v,
        (WS_CHILD | WS_VISIBLE | WS_TABSTOP) as u32,
        150,
        45,
        52,
        14,
        IDCANCEL as u16,
        0x0080,
        "Cancel",
    );
    unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(null()),
            v.as_ptr() as *const DLGTEMPLATE,
            with(|s| s.ctl),
            Some(spark_dialog_proc),
            0,
        );
    }
    SPARK_DIALOG.with(|x| x.borrow_mut().1.take())
}

/// A small yellow dot next to the item (a native menu can show an icon but not a coloured label).
/// Bright when the option is on, faint when off. Both bitmaps are made once.
unsafe fn set_menu_dot(menu: HMENU, id: usize, on: bool) {
    thread_local! { static DOTS: Cell<[HBITMAP; 2]> = const { Cell::new([null_mut(); 2]) }; }
    let mut dots = DOTS.with(|d| d.get());
    if dots[on as usize].is_null() {
        dots[on as usize] = make_dot(if on { 1.0 } else { 0.35 });
        DOTS.with(|d| d.set(dots));
    }
    let bmp = dots[on as usize];
    if bmp.is_null() {
        return;
    }
    let mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_BITMAP,
        hbmpItem: bmp,
        ..std::mem::zeroed()
    };
    SetMenuItemInfoW(menu, id as u32, 0, &mii);
}

unsafe fn make_dot(opacity: f32) -> HBITMAP {
    const N: i32 = 16;
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: N,
            biHeight: -N,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };
    let mut bits: *mut std::ffi::c_void = null_mut();
    let bmp = CreateDIBSection(null_mut(), &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if bmp.is_null() || bits.is_null() {
        return null_mut();
    }
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (N * N) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (
                x as f32 + 0.5 - N as f32 / 2.0,
                y as f32 + 0.5 - N as f32 / 2.0,
            );
            let a =
                (N as f32 / 2.0 - 2.0 - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0) * opacity;
            // Premultiplied ARGB: yellow (255, 200, 0).
            let c = |v: f32| (v * a) as u32;
            px[(y * N + x) as usize] = (c(255.0) << 24) | (c(255.0) << 16) | (c(200.0) << 8);
        }
    }
    bmp
}

/// Menus have no tooltips of their own, so this floats one next to the hovered item.
unsafe fn show_menu_tip(menu: HMENU, text: &str) {
    let ctl = with(|s| s.ctl);
    let mut tip = with(|s| s.menu_tip);
    if tip.is_null() {
        tip = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ctl,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        with(|s| s.menu_tip = tip);
    }
    let mut t = wide(text);
    let ti = TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_TRACK | TTF_ABSOLUTE,
        hwnd: ctl,
        uId: 1,
        lpszText: t.as_mut_ptr(),
        ..std::mem::zeroed()
    };
    let tool = &ti as *const _ as LPARAM;
    SendMessageW(tip, TTM_DELTOOLW, 0, tool);
    SendMessageW(tip, TTM_ADDTOOLW, 0, tool);
    // Beside the item, on the side with room.
    let mut item = std::mem::zeroed::<RECT>();
    for i in 0..GetMenuItemCount(menu) {
        if GetMenuItemID(menu, i) as usize == ID_FAKE_DATA {
            GetMenuItemRect(null_mut(), menu, i as u32, &mut item);
        }
    }
    let size = SendMessageW(tip, TTM_GETBUBBLESIZE, 0, tool) as u32;
    let width = (size & 0xFFFF) as i32;
    let x = if item.right + 4 + width <= GetSystemMetrics(SM_CXSCREEN) {
        item.right + 4
    } else {
        item.left - 4 - width
    };
    SendMessageW(
        tip,
        TTM_TRACKPOSITION,
        0,
        ((item.top as u32 & 0xFFFF) << 16 | (x as u32 & 0xFFFF)) as LPARAM,
    );
    SendMessageW(tip, TTM_TRACKACTIVATE, 1, tool);
}

unsafe fn hide_menu_tip() {
    let tip = with(|s| s.menu_tip);
    if !tip.is_null() {
        let ti = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            hwnd: with(|s| s.ctl),
            uId: 1,
            ..std::mem::zeroed()
        };
        SendMessageW(tip, TTM_TRACKACTIVATE, 0, &ti as *const _ as LPARAM);
    }
}

unsafe fn show_menu() {
    let ctl = with(|s| s.ctl);
    let menu = CreatePopupMenu();
    // Settings first, then the everyday actions next to Exit, closest to the
    // taskbar where the menu opens.
    let tick = |id: usize| {
        if setting_checked(id) == Some(true) {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        }
    };
    let sub = CreatePopupMenu();
    for (i, (_, label)) in INTERVALS.iter().enumerate() {
        AppendMenuW(
            sub,
            tick(ID_INTERVAL_BASE + i),
            ID_INTERVAL_BASE + i,
            wide(label).as_ptr(),
        );
    }
    AppendMenuW(
        menu,
        tick(ID_SHOW_CLAUDE),
        ID_SHOW_CLAUDE,
        w!("Show Claude"),
    );
    AppendMenuW(menu, tick(ID_SHOW_CODEX), ID_SHOW_CODEX, w!("Show Codex"));
    AppendMenuW(menu, tick(ID_SHOW_SPARK), ID_SHOW_SPARK, w!("Show Spark"));
    AppendMenuW(menu, tick(ID_ALERTS), ID_ALERTS, w!("Alerts"));
    AppendMenuW(menu, MF_STRING, ID_SPARK_ADDRESS, w!("Spark address..."));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_POPUP, sub as usize, w!("Update every"));
    AppendMenuW(
        menu,
        tick(ID_SHOW_LEFT),
        ID_SHOW_LEFT,
        w!("Show remaining instead of used"),
    );
    let custom = CreatePopupMenu();
    let fonts = CreatePopupMenu();
    for (i, (_, label)) in FONT_SIZES.iter().enumerate() {
        AppendMenuW(
            fonts,
            tick(ID_FONT_BASE + i),
            ID_FONT_BASE + i,
            wide(label).as_ptr(),
        );
    }
    AppendMenuW(custom, MF_POPUP, fonts as usize, w!("Font size"));
    let icon = CreatePopupMenu();
    AppendMenuW(icon, tick(ID_HIDE_ICON), ID_HIDE_ICON, w!("Hide icon"));
    AppendMenuW(
        icon,
        tick(ID_CLAUDE_ICON),
        ID_CLAUDE_ICON,
        w!("Show Claude icon"),
    );
    AppendMenuW(
        icon,
        tick(ID_PIXEL_ICON),
        ID_PIXEL_ICON,
        w!("Show pixel Claude icon"),
    );
    AppendMenuW(icon, MF_SEPARATOR, 0, null());
    let sizes = CreatePopupMenu();
    for (i, (_, label)) in ICON_SIZES.iter().enumerate() {
        AppendMenuW(
            sizes,
            tick(ID_ICON_SIZE_BASE + i),
            ID_ICON_SIZE_BASE + i,
            wide(label).as_ptr(),
        );
    }
    AppendMenuW(icon, MF_POPUP, sizes as usize, w!("Icon size"));
    AppendMenuW(custom, MF_POPUP, icon as usize, w!("Icon"));
    let rings = CreatePopupMenu();
    AppendMenuW(rings, tick(ID_RINGS), ID_RINGS, w!("Show usage rings"));
    AppendMenuW(rings, MF_SEPARATOR, 0, null());
    AppendMenuW(
        rings,
        tick(ID_RING_TEXT),
        ID_RING_TEXT,
        w!("Numbers inside the rings"),
    );
    AppendMenuW(
        rings,
        tick(ID_RING_COLOR_ONLY),
        ID_RING_COLOR_ONLY,
        w!("Warning colors on the rings only"),
    );
    AppendMenuW(
        rings,
        tick(ID_RING_SPACE),
        ID_RING_SPACE,
        w!("Space around the rings (small taskbars)"),
    );
    let margins = CreatePopupMenu();
    for (i, px) in RING_MARGINS.iter().enumerate() {
        let text = wide(&format!("{px} px"));
        AppendMenuW(
            margins,
            tick(ID_RING_MARGIN_BASE + i),
            ID_RING_MARGIN_BASE + i,
            text.as_ptr(),
        );
    }
    AppendMenuW(rings, MF_POPUP, margins as usize, w!("Number margin"));
    AppendMenuW(custom, MF_POPUP, rings as usize, w!("Usage rings"));
    AppendMenuW(
        custom,
        tick(ID_ROLL),
        ID_ROLL,
        w!("Roll numbers when they change"),
    );
    AppendMenuW(
        custom,
        tick(ID_HIDE_LABELS),
        ID_HIDE_LABELS,
        w!("Hide labels"),
    );
    AppendMenuW(
        custom,
        tick(ID_HIDE_PERCENT),
        ID_HIDE_PERCENT,
        w!("Hide % symbol"),
    );
    let limits = CreatePopupMenu();
    AppendMenuW(limits, tick(ID_HIDE_FIVE), ID_HIDE_FIVE, w!("5-hour"));
    AppendMenuW(limits, tick(ID_HIDE_WEEK), ID_HIDE_WEEK, w!("Weekly"));
    AppendMenuW(
        limits,
        tick(ID_HIDE_FABLE),
        ID_HIDE_FABLE,
        w!("Fable (weekly)"),
    );
    AppendMenuW(custom, MF_POPUP, limits as usize, w!("Show/hide limits"));
    // On a vertical taskbar the same setting moves the widget to the top.
    let vertical = with(|s| s.rendered.as_ref().is_some_and(|k| k.vertical));
    let side = if vertical {
        w!("Show on the top side")
    } else {
        w!("Show on the left side")
    };
    AppendMenuW(custom, tick(ID_ON_LEFT), ID_ON_LEFT, side);
    // Only matters with more than one account.
    let n_accounts = with(|s| s.accounts.len());
    let per = CreatePopupMenu();
    for n in 1..=render::MAX_ROWS_PER_COLUMN {
        let text = wide(&n.to_string());
        AppendMenuW(
            per,
            tick(ID_PER_COL_BASE + n),
            ID_PER_COL_BASE + n,
            text.as_ptr(),
        );
    }
    let per_flags = if n_accounts > 1 {
        MF_POPUP
    } else {
        MF_POPUP | MF_GRAYED
    };
    AppendMenuW(custom, per_flags, per as usize, w!("Accounts per column"));
    AppendMenuW(menu, MF_POPUP, custom as usize, w!("Customize"));
    let accounts = CreatePopupMenu();
    AppendMenuW(accounts, MF_STRING, ID_ADD_ACCOUNT, w!("Add account…"));
    let labels = with(|s| {
        s.accounts
            .iter()
            .map(|a| a.label.clone())
            .collect::<Vec<_>>()
    });
    if !labels.is_empty() {
        AppendMenuW(accounts, MF_SEPARATOR, 0, null());
    }
    // Keep at least one account.
    let removable = if labels.len() > 1 {
        MF_STRING
    } else {
        MF_STRING | MF_GRAYED
    };
    for (i, label) in labels.iter().enumerate() {
        let text = wide(&format!("Remove {}", label.replace('&', "&&")));
        AppendMenuW(accounts, removable, ID_REMOVE_BASE + i, text.as_ptr());
    }
    AppendMenuW(menu, MF_POPUP, accounts as usize, w!("Accounts"));
    AppendMenuW(
        menu,
        tick(ID_AUTOSTART),
        ID_AUTOSTART,
        w!("Start with Windows"),
    );
    if DEV {
        AppendMenuW(
            menu,
            tick(ID_FAKE_DATA),
            ID_FAKE_DATA,
            w!("Use fake data (dev only)"),
        );
        set_menu_dot(
            menu,
            ID_FAKE_DATA,
            setting_checked(ID_FAKE_DATA) == Some(true),
        );
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_REFRESH, w!("Refresh now"));
    AppendMenuW(menu, MF_STRING, ID_OPEN, w!("Open usage page"));
    // Only when claude-swap was installed at launch.
    let (cswap, running) = with(|s| (s.cswap.clone(), s.cswap_running));
    if cswap.is_some() {
        let (flags, text) = if running {
            (MF_STRING | MF_GRAYED, w!("Switching account…"))
        } else {
            (MF_STRING, w!("Switch to next account (cswap)"))
        };
        AppendMenuW(menu, flags, ID_CSWAP, text);
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));
    refresh_menu(menu);
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(ctl);
    // Settings apply in place and leave the menu open; see `menu_filter`.
    MENU.set(menu);
    let hook = SetWindowsHookExW(
        WH_MSGFILTER,
        Some(menu_filter),
        null_mut(),
        GetCurrentThreadId(),
    );
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        ctl,
        null(),
    );
    UnhookWindowsHookEx(hook);
    MENU.set(null_mut());
    PostMessageW(ctl, WM_NULL, 0, 0);
    DestroyMenu(menu);
    // Enter on a setting still closes the menu and lands here.
    if apply_setting(cmd as usize) {
        return;
    }
    match cmd as usize {
        ID_REFRESH => {
            start_fetch_all();
            start_provider_fetch();
        }
        ID_SPARK_ADDRESS => {
            if let Some(host) = prompt_spark_address() {
                with(|s| {
                    s.spark_host = host.clone();
                    s.rendered = None;
                });
                write_string_setting("SparkHost", &host);
                start_provider_fetch();
            }
        }
        ID_ADD_ACCOUNT => add_account(),
        id if (ID_REMOVE_BASE..ID_REMOVE_BASE + labels.len()).contains(&id) => {
            remove_account(id - ID_REMOVE_BASE);
        }
        ID_OPEN => open_usage_page(),
        ID_CSWAP => {
            if let Some(exe) = cswap {
                run_cswap_switch(exe);
            }
        }
        ID_EXIT => quit(),
        _ => {}
    }
}

thread_local! {
    /// The context menu while it is open.
    static MENU: Cell<HMENU> = const { Cell::new(null_mut()) };
}

/// Whether a setting's menu item is ticked, or None for items that are not settings.
fn setting_checked(id: usize) -> Option<bool> {
    if id == ID_AUTOSTART {
        return Some(autostart_enabled());
    }
    with(|s| {
        Some(match id {
            ID_SHOW_LEFT => s.show_left,
            ID_SHOW_CLAUDE => s.show_claude,
            ID_SHOW_CODEX => s.show_codex,
            ID_SHOW_SPARK => s.show_spark,
            ID_ALERTS => s.alerts,
            ID_RINGS => s.rings,
            ID_RING_TEXT => s.ring_text,
            ID_RING_COLOR_ONLY => s.ring_color_only,
            ID_RING_SPACE => s.ring_space,
            ID_ROLL => s.roll,
            ID_HIDE_LABELS => s.hide_labels,
            ID_HIDE_PERCENT => s.hide_percent,
            ID_FAKE_DATA => s.fake_data,
            ID_ON_LEFT => s.on_left,
            // Ticked limits are shown.
            ID_HIDE_FIVE => !s.hide_five,
            ID_HIDE_WEEK => !s.hide_week,
            ID_HIDE_FABLE => !s.hide_fable,
            ID_HIDE_ICON => s.hide_icon,
            ID_CLAUDE_ICON => !s.hide_icon && !s.pixel_icon,
            ID_PIXEL_ICON => !s.hide_icon && s.pixel_icon,
            id if (ID_PER_COL_BASE + 1..=ID_PER_COL_BASE + render::MAX_ROWS_PER_COLUMN)
                .contains(&id) =>
            {
                s.rows_per_col as usize == id - ID_PER_COL_BASE
            }
            id if (ID_RING_MARGIN_BASE..ID_RING_MARGIN_BASE + RING_MARGINS.len()).contains(&id) => {
                s.ring_margin == RING_MARGINS[id - ID_RING_MARGIN_BASE]
            }
            id if (ID_FONT_BASE..ID_FONT_BASE + FONT_SIZES.len()).contains(&id) => {
                s.font_size == FONT_SIZES[id - ID_FONT_BASE].0
            }
            id if (ID_ICON_SIZE_BASE..ID_ICON_SIZE_BASE + ICON_SIZES.len()).contains(&id) => {
                s.icon_size == ICON_SIZES[id - ID_ICON_SIZE_BASE].0
            }
            id if (ID_INTERVAL_BASE..ID_INTERVAL_BASE + INTERVALS.len()).contains(&id) => {
                s.interval == INTERVALS[id - ID_INTERVAL_BASE].0
            }
            _ => return None,
        })
    })
}

/// Applies a setting picked in the menu. Returns false for other items.
fn apply_setting(id: usize) -> bool {
    match id {
        ID_SHOW_CLAUDE | ID_SHOW_CODEX | ID_SHOW_SPARK => {
            let (name, on) = with(|s| match id {
                ID_SHOW_CLAUDE => {
                    s.show_claude = !s.show_claude;
                    ("ShowClaude", s.show_claude)
                }
                ID_SHOW_CODEX => {
                    s.show_codex = !s.show_codex;
                    ("ShowCodex", s.show_codex)
                }
                _ => {
                    s.show_spark = !s.show_spark;
                    ("ShowSpark", s.show_spark)
                }
            });
            write_setting(name, on as u32);
            ensure_widget();
        }
        ID_ALERTS => {
            let enabled = with(|s| {
                s.alerts = !s.alerts;
                s.alerts
            });
            write_setting("Alerts", enabled as u32);
        }
        ID_AUTOSTART => set_autostart(!autostart_enabled()),
        ID_SHOW_LEFT => {
            let on = with(|s| {
                s.show_left = !s.show_left;
                s.show_left
            });
            write_setting("ShowRemaining", on as u32);
            ensure_widget();
        }
        ID_RINGS => {
            let on = with(|s| {
                s.rings = !s.rings;
                s.rings
            });
            write_setting("Rings", on as u32);
            ensure_widget();
        }
        ID_RING_SPACE => {
            let on = with(|s| {
                s.ring_space = !s.ring_space;
                s.ring_space
            });
            write_setting("RingSpace", on as u32);
            ensure_widget();
        }
        ID_RING_COLOR_ONLY => {
            let on = with(|s| {
                s.ring_color_only = !s.ring_color_only;
                s.ring_color_only
            });
            write_setting("RingColorOnly", on as u32);
            ensure_widget();
        }
        ID_RING_TEXT => {
            let on = with(|s| {
                s.ring_text = !s.ring_text;
                s.ring_text
            });
            write_setting("RingText", on as u32);
            ensure_widget();
        }
        ID_ROLL => {
            let on = with(|s| {
                s.roll = !s.roll;
                s.roll
            });
            write_setting("RollNumbers", on as u32);
        }
        ID_HIDE_FIVE => {
            let on = with(|s| {
                s.hide_five = !s.hide_five;
                s.hide_five
            });
            write_setting("HideFiveHour", on as u32);
            ensure_widget();
        }
        ID_HIDE_WEEK => {
            let on = with(|s| {
                s.hide_week = !s.hide_week;
                s.hide_week
            });
            write_setting("HideWeekly", on as u32);
            ensure_widget();
        }
        ID_HIDE_FABLE => {
            let on = with(|s| {
                s.hide_fable = !s.hide_fable;
                s.hide_fable
            });
            write_setting("HideFable", on as u32);
            ensure_widget();
        }
        ID_ON_LEFT => {
            let on = with(|s| {
                s.on_left = !s.on_left;
                s.on_left
            });
            write_setting("OnLeft", on as u32);
            ensure_widget();
        }
        ID_HIDE_LABELS => {
            let on = with(|s| {
                s.hide_labels = !s.hide_labels;
                s.hide_labels
            });
            write_setting("HideLabels", on as u32);
            ensure_widget();
        }
        ID_FAKE_DATA if DEV => {
            let on = with(|s| {
                s.fake_data = !s.fake_data;
                s.fake_data
            });
            write_setting("FakeData", on as u32);
            // Switch sources right away instead of at the next timer.
            start_fetch_all();
        }
        ID_HIDE_PERCENT => {
            let on = with(|s| {
                s.hide_percent = !s.hide_percent;
                s.hide_percent
            });
            write_setting("HidePercent", on as u32);
            ensure_widget();
        }
        id @ (ID_HIDE_ICON | ID_CLAUDE_ICON | ID_PIXEL_ICON) => {
            let (hide, pixel) = (id == ID_HIDE_ICON, id == ID_PIXEL_ICON);
            with(|s| {
                s.hide_icon = hide;
                // Hiding keeps the chosen mark for when it is shown again.
                if !hide {
                    s.pixel_icon = pixel;
                }
            });
            write_setting("HideIcon", hide as u32);
            if !hide {
                write_setting("PixelIcon", pixel as u32);
            }
            ensure_widget();
        }
        id if (ID_PER_COL_BASE + 1..=ID_PER_COL_BASE + render::MAX_ROWS_PER_COLUMN)
            .contains(&id) =>
        {
            let n = (id - ID_PER_COL_BASE) as u32;
            with(|s| s.rows_per_col = n);
            write_setting("AccountsPerColumn", n);
            ensure_widget();
            update_tooltip();
        }
        id if (ID_RING_MARGIN_BASE..ID_RING_MARGIN_BASE + RING_MARGINS.len()).contains(&id) => {
            let px = RING_MARGINS[id - ID_RING_MARGIN_BASE];
            with(|s| s.ring_margin = px);
            write_setting("RingMargin", px);
            ensure_widget();
        }
        id if (ID_ICON_SIZE_BASE..ID_ICON_SIZE_BASE + ICON_SIZES.len()).contains(&id) => {
            let px = ICON_SIZES[id - ID_ICON_SIZE_BASE].0;
            with(|s| s.icon_size = px);
            write_setting("IconSize", px);
            ensure_widget();
        }
        id if (ID_FONT_BASE..ID_FONT_BASE + FONT_SIZES.len()).contains(&id) => {
            let px = FONT_SIZES[id - ID_FONT_BASE].0;
            with(|s| s.font_size = px);
            write_setting("FontSize", px);
            ensure_widget();
        }
        id if (ID_INTERVAL_BASE..ID_INTERVAL_BASE + INTERVALS.len()).contains(&id) => {
            set_interval(INTERVALS[id - ID_INTERVAL_BASE].0);
        }
        _ => return false,
    }
    true
}

/// Brings the ticks up to date, and greys out the ring options without rings.
unsafe fn refresh_menu(menu: HMENU) {
    for i in 0..GetMenuItemCount(menu) {
        let sub = GetSubMenu(menu, i);
        if !sub.is_null() {
            refresh_menu(sub);
            continue;
        }
        let id = GetMenuItemID(menu, i) as usize;
        if let Some(on) = setting_checked(id) {
            CheckMenuItem(
                menu,
                i as u32,
                MF_BYPOSITION | if on { MF_CHECKED } else { MF_UNCHECKED },
            );
            if id == ID_FAKE_DATA {
                set_menu_dot(menu, id, on);
            }
        }
        if (ID_RING_MARGIN_BASE..ID_RING_MARGIN_BASE + RING_MARGINS.len()).contains(&id) {
            let on = with(|s| s.rings && s.ring_text);
            EnableMenuItem(
                menu,
                i as u32,
                MF_BYPOSITION | if on { MF_ENABLED } else { MF_GRAYED },
            );
        }
        if id == ID_RING_TEXT || id == ID_RING_COLOR_ONLY || id == ID_RING_SPACE {
            // Space around the rings only applies with the numbers inside them.
            let on = with(|s| s.rings && (id != ID_RING_SPACE || s.ring_text));
            EnableMenuItem(
                menu,
                i as u32,
                MF_BYPOSITION | if on { MF_ENABLED } else { MF_GRAYED },
            );
        }
    }
}

/// Runs inside the menu's loop. A click on a setting applies it and updates
/// the open menu instead of closing it; any other click goes on as usual.
unsafe extern "system" fn menu_filter(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let m = &*(lp as *const MSG);
    let up = matches!(m.message, WM_LBUTTONUP | WM_RBUTTONUP);
    let dbl = matches!(m.message, WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK);
    if code == MSGF_MENU as i32 && (up || dbl) {
        let hwnd = WindowFromPoint(m.pt);
        let mut class = [0u16; 8];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        // Open menus are windows of class "#32768".
        if String::from_utf16_lossy(&class[..n]) == "#32768" {
            let menu = SendMessageW(hwnd, MN_GETHMENU, 0, 0) as HMENU;
            let i = MenuItemFromPoint(null_mut(), menu, m.pt);
            let usable = i >= 0
                && GetMenuState(menu, i as u32, MF_BYPOSITION) & (MF_GRAYED | MF_DISABLED) == 0;
            let id = if usable {
                GetMenuItemID(menu, i) as usize
            } else {
                0
            };
            if dbl && setting_checked(id).is_some() {
                // A quick second click arrives as a double-click, which the menu
                // takes as a choice and closes; its button-up toggles again.
                return 1;
            }
            if up && apply_setting(id) {
                refresh_menu(MENU.get());
                EnumThreadWindows(GetCurrentThreadId(), Some(repaint_menu), 0);
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}

unsafe extern "system" fn repaint_menu(hwnd: HWND, _: LPARAM) -> BOOL {
    InvalidateRect(hwnd, null(), 1);
    1
}

// ---------- window procs ----------

unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let tb_msg = with(|s| s.taskbar_created_msg);
    match msg {
        WM_TIMER => {
            match wp {
                T_LAYOUT => ensure_widget(),
                T_ROLL => {
                    let animating = with(|s| {
                        for a in &mut s.accounts {
                            a.prev = a
                                .prev
                                .take()
                                .filter(|(_, at)| at.elapsed().as_secs_f32() < ROLL_SECS);
                            for started in &mut a.critical_since {
                                if started.is_some_and(|at| at.elapsed().as_secs_f32() >= 1.8) {
                                    *started = None;
                                }
                            }
                        }
                        for started in &mut s.codex_critical_since {
                            if started.is_some_and(|at| at.elapsed().as_secs_f32() >= 1.8) {
                                *started = None;
                            }
                        }
                        s.accounts.iter().any(|a| a.prev.is_some())
                            || s.accounts
                                .iter()
                                .any(|a| a.critical_since.iter().any(Option::is_some))
                            || s.codex_critical_since.iter().any(Option::is_some)
                            || s.accounts.iter().any(|a| a.usage.is_none() && a.fetching)
                            || s.codex_checked_at == 0
                            || (!s.spark_host.is_empty() && s.spark.is_none())
                    });
                    if !animating {
                        KillTimer(hwnd, T_ROLL);
                    }
                    ensure_widget(); // the next frame, or the settled numbers
                }
                T_PROVIDER => {
                    start_provider_fetch();
                    SetTimer(hwnd, T_PROVIDER, provider_poll_ms(), None);
                }
                T_SPARK_PULSE => {
                    let busy = with(|s| s.spark.as_ref().is_some_and(|spark| spark.busy));
                    if busy {
                        with(|s| s.rendered = None);
                        ensure_widget();
                    } else {
                        KillTimer(hwnd, T_SPARK_PULSE);
                    }
                }
                T_FLYOUT => {
                    let widget_hover = with(|s| s.hover);
                    let flyout_hover = flyout::is_hovered();
                    let persistent = flyout::is_persistent();
                    if widget_hover || flyout_hover || persistent {
                        with(|s| s.leave_since = None);
                        let ready = with(|s| {
                            persistent
                                || flyout::is_open()
                                || s.hover_since
                                    .is_some_and(|at| at.elapsed().as_millis() >= 350)
                        });
                        let redraw = with(|s| {
                            s.last_flyout_draw
                                .is_none_or(|at| at.elapsed().as_secs_f32() >= 1.0)
                        });
                        if ready && redraw {
                            update_flyout();
                        }
                    } else {
                        let close = with(|s| {
                            let left = s.leave_since.get_or_insert_with(Instant::now);
                            left.elapsed().as_millis() >= 400
                        });
                        if close {
                            flyout::hide();
                            if !flyout::is_open() {
                                KillTimer(hwnd, T_FLYOUT);
                            }
                        }
                    }
                }
                T_ALERT_REMOVE => {
                    remove_alert();
                    KillTimer(hwnd, T_ALERT_REMOVE);
                }
                T_TICK => {
                    update_tooltip();
                    // A window just reset: fetch now instead of waiting.
                    // Not while the last fetch failed: that would retry every
                    // 30 seconds and skip the backoff set in on_result.
                    let reset_passed: Vec<u32> = with(|s| {
                        s.accounts
                            .iter()
                            .filter(|a| {
                                if a.error.is_some() {
                                    return false;
                                }
                                let u = a.usage.clone().unwrap_or_default();
                                [u.five_hour, u.weekly, u.fable].iter().flatten().any(|l| {
                                    l.resets_at.is_some_and(|t| {
                                        t < now() && a.last_ok.is_some_and(|ok| ok < t)
                                    })
                                })
                            })
                            .map(|a| a.id)
                            .collect()
                    });
                    for id in reset_passed {
                        start_fetch(id);
                    }
                }
                id if id >= T_FETCH_BASE => start_fetch((id - T_FETCH_BASE) as u32),
                _ => {}
            }
            0
        }
        WM_MENUSELECT if DEV => {
            let id = (wp & 0xFFFF) as usize;
            let flags = ((wp >> 16) & 0xFFFF) as u32;
            if id == ID_FAKE_DATA && flags & MF_POPUP == 0 && flags != 0xFFFF {
                show_menu_tip(lp as HMENU, "Prevents rate limits in dev");
            } else {
                hide_menu_tip();
            }
            0
        }
        WM_APP_CSWAP => {
            let r = Box::from_raw(lp as *mut Result<(), String>);
            on_cswap_done(*r);
            0
        }
        WM_APP_PROVIDER => {
            let r = Box::from_raw(lp as *mut ProviderResult);
            on_provider_result(*r);
            0
        }
        WM_APP_RESULT => {
            let r = Box::from_raw(lp as *mut FetchResult);
            on_result(*r);
            0
        }
        flyout::WM_REFRESH => {
            start_fetch_all();
            start_provider_fetch();
            0
        }
        flyout::WM_OPEN_PROVIDER => {
            match wp {
                value if value == flyout::ProviderKind::Claude as usize => {
                    open_url("https://claude.ai/settings/usage");
                }
                value if value == flyout::ProviderKind::Codex as usize => {
                    open_url("https://chatgpt.com/codex/settings/usage");
                }
                value if value == flyout::ProviderKind::Spark as usize => {
                    let host = with(|state| state.spark_host.clone());
                    if !host.is_empty() {
                        open_url(&format!("http://{host}/"));
                    }
                }
                _ => {}
            }
            0
        }
        flyout::WM_DISMISSED => {
            with(|state| {
                state.hover_since = None;
                state.leave_since = None;
                state.last_flyout_draw = None;
            });
            KillTimer(hwnd, T_FLYOUT);
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            with(|s| s.dark = system_dark());
            ensure_widget();
            0
        }
        WM_POWERBROADCAST => {
            if wp as u32 == PBT_APMRESUMEAUTOMATIC {
                for id in with(|s| s.accounts.iter().map(|a| a.id).collect::<Vec<_>>()) {
                    SetTimer(hwnd, T_FETCH_BASE + id as usize, 5000, None);
                }
            }
            1
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ if msg == tb_msg && msg != 0 => {
            // Explorer restarted: our old widget died with the old taskbar.
            with(|s| s.widget = null_mut());
            ensure_widget();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn widget_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_MOUSEMOVE => {
            if !with(|s| std::mem::replace(&mut s.hover, true)) {
                with(|s| {
                    s.hover_since = Some(Instant::now());
                    s.leave_since = None;
                });
                let ctl = with(|s| s.ctl);
                SetTimer(ctl, T_FLYOUT, 100, None);
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
                ensure_widget();
            }
            0
        }
        WM_MOUSELEAVE => {
            with(|s| {
                s.hover = false;
                s.hover_since = None;
                s.leave_since = Some(Instant::now());
            });
            ensure_widget();
            0
        }
        WM_LBUTTONUP => {
            let ctl = with(|state| state.ctl);
            if flyout::is_open() && flyout::is_persistent() {
                flyout::close();
                with(|state| {
                    state.hover_since = None;
                    state.leave_since = None;
                    state.last_flyout_draw = None;
                });
                KillTimer(ctl, T_FLYOUT);
            } else {
                show_flyout(true);
                SetTimer(ctl, T_FLYOUT, 100, None);
            }
            0
        }
        WM_RBUTTONUP => {
            show_menu();
            0
        }
        WM_DESTROY => {
            with(|s| {
                if s.widget == hwnd {
                    s.widget = null_mut();
                    s.tip = null_mut();
                }
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
