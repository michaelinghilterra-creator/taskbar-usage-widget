use crate::http::wide;
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use std::time::Instant;
use windows_sys::core::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_REFRESH: u32 = WM_APP + 30;
pub const WM_OPEN_PROVIDER: u32 = WM_APP + 31;
pub const WM_DISMISSED: u32 = WM_APP + 32;
const CLASS: *const u16 = w!("TaskbarUsageFlyout");
const WM_MOUSE_LEAVE_MSG: u32 = 0x02A3;
const MIN_WIDTH: f32 = 300.0;
const VERTICAL_MIN_WIDTH: f32 = 360.0;
const MAX_WIDTH: f32 = 560.0;
const WORK_MARGIN: f32 = 12.0;
const ANIMATION_TIMER: usize = 1;

#[repr(usize)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    #[default]
    Claude,
    Codex,
    Spark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderHit {
    Close,
    Pin,
    Refresh,
    Provider(ProviderKind),
    None,
}

#[derive(Clone, Default, PartialEq)]
pub struct Bar {
    pub label: String,
    pub percent: f64,
    pub previous_percent: Option<f64>,
    pub number_progress: f32,
    pub meter_percent: Option<f64>,
    pub elapsed: f64,
    pub reset: String,
    pub ahead_by: f64,
    pub projected: f64,
    pub full_at: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
pub struct AuxStatus {
    pub name: String,
    pub up: bool,
    pub busy: bool,
}

#[derive(Clone, Default, PartialEq)]
pub struct SparkStats {
    pub tok_s: f64,
    pub peak: f64,
    pub running: i64,
    pub waiting: i64,
    pub kv_pct: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub gpu_util: f64,
    pub gpu_temp: Option<f64>,
    pub gpu_power: Option<f64>,
    pub mem_pct: Option<f64>,
    pub swap_pct: Option<f64>,
    pub aux: Vec<AuxStatus>,
}

#[derive(Clone, Default, PartialEq)]
pub struct Section {
    pub provider: ProviderKind,
    pub title: String,
    pub chip: String,
    pub bars: Vec<Bar>,
    pub chart: Vec<f64>,
    pub today: String,
    pub as_of: String,
    pub error: Option<String>,
    pub stale: bool,
    pub spark: Option<SparkStats>,
    pub loading: bool,
    pub empty: bool,
}

#[derive(Clone, Default, PartialEq)]
pub struct Snapshot {
    pub sections: Vec<Section>,
    pub updated: String,
    pub show_left: bool,
    pub pulse_alpha: f32,
}

#[allow(dead_code)]
fn worst_limit_index(bars: &[Bar]) -> Option<usize> {
    bars.iter()
        .enumerate()
        .max_by_key(|(_, bar)| {
            let state = crate::render::strip_item_state(
                Some(bar.percent),
                Some((bar.elapsed / 100.0) as f32),
                Some(bar.projected as f32),
                bar.full_at.is_some(),
            );
            match state {
                crate::render::ItemState::Ok => 0,
                crate::render::ItemState::Warn => 1,
                crate::render::ItemState::Crit => 2,
            }
        })
        .map(|(index, _)| index)
}

#[allow(dead_code)]
fn provider_status_text(provider: &str, bars: &[Bar]) -> String {
    let Some(bar) = worst_limit_index(bars).and_then(|index| bars.get(index)) else {
        return format!("{provider} · No readings");
    };
    if let Some(time) = &bar.full_at {
        format!("{provider} · Runs out {time}")
    } else {
        format!("{provider} · On pace")
    }
}

#[allow(dead_code)]
fn age_copy(seconds: i64, stale: bool) -> String {
    let seconds = seconds.max(0);
    let value = if seconds >= 3600 {
        format!("{} h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{} m", seconds / 60)
    } else {
        format!("{} s", seconds)
    };
    format!("{value} {}", if stale { "old" } else { "ago" })
}

#[allow(dead_code)]
fn reset_time_copy(today: bool, weekday: &str, time: &str) -> String {
    if today {
        time.to_owned()
    } else {
        format!("{weekday} {time}")
    }
}

#[derive(Clone)]
struct View {
    snapshot: Snapshot,
    previous: Option<Snapshot>,
    changed_at: Instant,
    dark: bool,
    dpi: u32,
    width: i32,
    target: POINT,
    opened_at: Instant,
}

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(null_mut()) };
    static OWNER: Cell<HWND> = const { Cell::new(null_mut()) };
    static HOVER: Cell<bool> = const { Cell::new(false) };
    static PINNED: Cell<bool> = const { Cell::new(false) };
    static CLICK_MODE: Cell<bool> = const { Cell::new(false) };
    static HOVER_HIT: Cell<HeaderHit> = const { Cell::new(HeaderHit::None) };
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    static CLOSING_AT: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

fn control_rects(width: i32, scale: f32) -> [(HeaderHit, RECT); 3] {
    let pad = (17.0 * scale).round() as i32;
    let size = (14.0 * scale).round() as i32;
    let gap = (10.0 * scale).round() as i32;
    let top = (17.0 * scale).round() as i32;
    let bottom = top + size;
    let close_right = width - pad;
    let close = RECT {
        left: close_right - size,
        top,
        right: close_right,
        bottom,
    };
    let pin = RECT {
        left: close.left - gap - size,
        top,
        right: close.left - gap,
        bottom,
    };
    let refresh = RECT {
        left: pin.left - gap - size,
        top,
        right: pin.left - gap,
        bottom,
    };
    [
        (HeaderHit::Close, close),
        (HeaderHit::Pin, pin),
        (HeaderHit::Refresh, refresh),
    ]
}

fn contains(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

pub fn header_hit_test(x: i32, y: i32, width: i32, scale: f32, sections: &[Section]) -> HeaderHit {
    for (hit, rect) in control_rects(width, scale) {
        if contains(rect, x, y) {
            return hit;
        }
    }
    let snapshot = Snapshot {
        sections: sections.to_vec(),
        ..Default::default()
    };
    let layout = unsafe { card_layout(&snapshot, width, scale) };
    for panel in layout.panels {
        let section = &sections[panel.index];
        let header = RECT {
            left: panel.rect.left,
            top: panel.rect.top,
            right: panel.rect.right,
            bottom: panel.rect.top + (40.0 * scale).round() as i32,
        };
        if contains(header, x, y) {
            return HeaderHit::Provider(section.provider);
        }
    }
    HeaderHit::None
}

pub unsafe fn register() {
    let class = WNDCLASSW {
        lpfnWndProc: Some(proc_),
        hInstance: GetModuleHandleW(null()),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        lpszClassName: CLASS,
        ..std::mem::zeroed()
    };
    RegisterClassW(&class);
}

pub fn is_hovered() -> bool {
    HOVER.with(Cell::get)
}
pub fn is_pinned() -> bool {
    PINNED.with(Cell::get)
}
pub fn is_click_mode() -> bool {
    CLICK_MODE.with(Cell::get)
}
pub fn is_persistent() -> bool {
    is_pinned() || is_click_mode()
}
pub fn is_open() -> bool {
    WINDOW.with(|window| unsafe { !window.get().is_null() && IsWindowVisible(window.get()) != 0 })
}
pub unsafe fn hide() {
    if !is_persistent() {
        begin_close();
    }
}

pub unsafe fn close() {
    PINNED.with(|pinned| pinned.set(false));
    CLICK_MODE.with(|mode| mode.set(false));
    HOVER_HIT.with(|hit| hit.set(HeaderHit::None));
    begin_close();
}

unsafe fn begin_close() {
    WINDOW.with(|window| {
        let hwnd = window.get();
        if !hwnd.is_null() && IsWindowVisible(hwnd) != 0 {
            CLOSING_AT.with(|closing| *closing.borrow_mut() = Some(Instant::now()));
            SetTimer(hwnd, ANIMATION_TIMER, 16, None);
        }
    });
}

pub unsafe fn show(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND) {
    show_impl(snapshot, anchor, dark, owner, is_click_mode());
}

pub unsafe fn show_click(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND) {
    CLICK_MODE.with(|mode| mode.set(true));
    show_impl(snapshot, anchor, dark, owner, true);
}

unsafe fn show_impl(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND, click: bool) {
    let hwnd = WINDOW.with(|slot| {
        let mut hwnd = slot.get();
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!("Usage details"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            slot.set(hwnd);
        }
        hwnd
    });
    OWNER.with(|slot| slot.set(owner));
    if hwnd.is_null() {
        return;
    }
    let dpi = GetDpiForWindow(owner).max(96);
    let pinned_rect = if is_pinned() && IsWindowVisible(hwnd) != 0 {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        Some(rect)
    } else {
        None
    };
    let width = pinned_rect
        .map(|rect| rect.right - rect.left)
        .unwrap_or_else(|| desired_width(anchor, dpi));
    let was_visible = IsWindowVisible(hwnd) != 0;
    CLOSING_AT.with(|closing| *closing.borrow_mut() = None);
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let style = if click {
        style & !WS_EX_NOACTIVATE
    } else {
        style | WS_EX_NOACTIVATE
    };
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
    SetWindowPos(
        hwnd,
        null_mut(),
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let frame = render_with_hover(
        snapshot,
        dark,
        dpi,
        is_pinned(),
        HOVER_HIT.with(Cell::get),
        width,
    );
    let point = pinned_rect.map_or_else(
        || position(anchor, frame.0, frame.1, dpi),
        |rect| POINT {
            x: rect.left,
            y: rect.top,
        },
    );
    let opened_at = VIEW.with(|view| {
        if was_visible {
            view.borrow()
                .as_ref()
                .map_or_else(Instant::now, |view| view.opened_at)
        } else {
            Instant::now()
        }
    });
    let (previous, changed_at) = VIEW.with(|view| {
        let view = view.borrow();
        match view.as_ref() {
            Some(view) if was_visible && view.snapshot != *snapshot => {
                (Some(view.snapshot.clone()), Instant::now())
            }
            Some(view) => (view.previous.clone(), view.changed_at),
            None => (None, Instant::now()),
        }
    });
    VIEW.with(|view| {
        *view.borrow_mut() = Some(View {
            snapshot: snapshot.clone(),
            previous,
            changed_at,
            dark,
            dpi,
            width,
            target: point,
            opened_at,
        });
    });
    let initial = if was_visible { 255 } else { 0 };
    let start = POINT {
        x: point.x,
        y: point.y
            + if was_visible {
                0
            } else {
                (8.0 * dpi as f32 / 96.0).round() as i32
            },
    };
    present(hwnd, start, frame, initial);
    if click {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    } else {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    SetTimer(hwnd, ANIMATION_TIMER, 16, None);
}

unsafe fn monitor_info(anchor: RECT) -> MONITORINFO {
    let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(monitor, &mut info);
    info
}

unsafe fn desired_width(anchor: RECT, dpi: u32) -> i32 {
    let info = monitor_info(anchor);
    let work = info.rcWork;
    let scale = dpi as f32 / 96.0;
    let margin = (WORK_MARGIN * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;
    let min = (VERTICAL_MIN_WIDTH * scale).round() as i32;
    let max = (MAX_WIDTH * scale).round() as i32;
    let horizontal = anchor.right - anchor.left >= anchor.bottom - anchor.top;
    if horizontal {
        return (anchor.right - anchor.left)
            .max((MIN_WIDTH * scale).round() as i32)
            .min(work.right - work.left);
    }
    let available = if horizontal {
        work.right - margin - anchor.left
    } else if anchor.right <= work.left + 4 {
        work.right - margin - anchor.right - gap
    } else if anchor.left >= work.right - 4 {
        anchor.left - gap - work.left - margin
    } else {
        work.right - margin - anchor.left
    };
    available.clamp(min, max)
}

unsafe fn position(anchor: RECT, width: i32, height: i32, dpi: u32) -> POINT {
    let info = monitor_info(anchor);
    let (work, screen) = (info.rcWork, info.rcMonitor);
    let scale = dpi as f32 / 96.0;
    let margin = (WORK_MARGIN * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;
    let horizontal = anchor.right - anchor.left >= anchor.bottom - anchor.top;
    if horizontal {
        let card_gap = (12.0 * scale).round() as i32;
        let x = if width > anchor.right - anchor.left {
            anchor.right - width
        } else {
            anchor.left
        };
        let y = if anchor.bottom <= work.top + 4 {
            anchor.bottom + card_gap
        } else {
            anchor.top - height - card_gap
        };
        return POINT {
            x: x.clamp(work.left, (work.right - width).max(work.left)),
            y: y.clamp(work.top, (work.bottom - height).max(work.top)),
        };
    }
    let available = work.right - margin - anchor.left;
    let mut x = if horizontal && available > width {
        work.right - margin - width
    } else {
        anchor.left
    };
    let mut y = anchor.top - height - gap;
    if anchor.bottom <= work.top + 4 {
        y = anchor.bottom + gap;
    } else if anchor.right <= work.left + 4 {
        x = anchor.right + gap;
        y = anchor.top;
    } else if anchor.left >= work.right - 4 {
        x = anchor.left - width - gap;
        y = anchor.top;
    } else if anchor.top < screen.top + height {
        y = anchor.bottom + gap;
    }
    let right_bound = if horizontal {
        work.right - margin
    } else {
        work.right
    };
    POINT {
        x: x.clamp(work.left, (right_bound - width).max(work.left)),
        y: y.clamp(work.top, (work.bottom - height).max(work.top)),
    }
}

pub fn render(snapshot: &Snapshot, dark: bool, dpi: u32, pinned: bool) -> (i32, i32, Vec<u32>) {
    let width = (MIN_WIDTH * dpi as f32 / 96.0).round() as i32;
    render_at_width(snapshot, dark, dpi, pinned, width)
}

pub fn render_at_width(
    snapshot: &Snapshot,
    dark: bool,
    dpi: u32,
    pinned: bool,
    width: i32,
) -> (i32, i32, Vec<u32>) {
    render_with_hover(snapshot, dark, dpi, pinned, HeaderHit::None, width)
}

fn render_with_hover(
    snapshot: &Snapshot,
    dark: bool,
    dpi: u32,
    pinned: bool,
    hovered: HeaderHit,
    width: i32,
) -> (i32, i32, Vec<u32>) {
    let scale = dpi as f32 / 96.0;
    let layout = unsafe { card_layout(snapshot, width, scale) };
    let height = layout.height;
    let mut pixels = vec![0u32; (width * height) as usize];
    let radius = (10.0 * scale) as i32;
    let pal = card_palette(dark);
    for y in 0..height {
        for x in 0..width {
            if rounded(x, y, width, height, radius) {
                let edge = !rounded(x - 1, y - 1, width - 2, height - 2, radius - 1);
                let color = if edge { pal.border } else { pal.card };
                pixels[(y * width + x) as usize] = (255u32 << 24) | colorref_to_argb(color);
            }
        }
    }
    unsafe {
        draw_e2_content(
            &mut pixels,
            width,
            height,
            snapshot,
            &layout,
            pal,
            scale,
            pinned,
            hovered,
        )
    };
    (width, height, pixels)
}

#[derive(Clone, Copy)]
struct CardPalette {
    card: COLORREF,
    border: COLORREF,
    panel: COLORREF,
    panel_border: COLORREF,
    text: COLORREF,
    muted: COLORREF,
    faint: COLORREF,
    track: COLORREF,
    elapsed: COLORREF,
    separator: COLORREF,
    chip: COLORREF,
    good: COLORREF,
    warn: COLORREF,
    crit: COLORREF,
    idle: COLORREF,
    claude: COLORREF,
    codex: COLORREF,
    spark: COLORREF,
    stale_fill: COLORREF,
}

fn rgba_color(value: crate::render::Rgba) -> COLORREF {
    rgb(
        (value.0 * 255.0).round() as u8,
        (value.1 * 255.0).round() as u8,
        (value.2 * 255.0).round() as u8,
    )
}

fn card_palette(dark: bool) -> CardPalette {
    let p = crate::render::palette(dark);
    CardPalette {
        card: rgba_color(p.card_background),
        border: rgba_color(p.card_border),
        panel: rgba_color(p.panel),
        panel_border: rgba_color(p.panel_border),
        text: rgba_color(p.card_text),
        muted: rgba_color(p.label),
        faint: rgba_color(p.faint),
        track: rgba_color(p.meter_track),
        elapsed: rgba_color(p.meter_elapsed),
        separator: rgba_color(p.separator),
        chip: rgba_color(p.card_background),
        good: rgba_color(p.good),
        warn: rgba_color(p.warn),
        crit: rgba_color(p.crit),
        idle: rgba_color(p.idle),
        claude: rgba_color(p.claude),
        codex: rgba_color(p.codex),
        spark: rgba_color(p.spark),
        stale_fill: if dark {
            rgb(44, 44, 49)
        } else {
            rgb(230, 230, 234)
        },
    }
}

fn colorref_to_argb(color: COLORREF) -> u32 {
    let red = color & 255;
    let green = (color >> 8) & 255;
    let blue = (color >> 16) & 255;
    (red << 16) | (green << 8) | blue
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PillState {
    Good,
    Warn,
    Crit,
    Idle,
    Spark,
}

struct PillLayout {
    text: String,
    state: PillState,
    rect: RECT,
}

struct PanelLayout {
    index: usize,
    rect: RECT,
}

struct CardLayout {
    height: i32,
    pills: Vec<PillLayout>,
    panels: Vec<PanelLayout>,
}

fn section_state(section: &Section) -> crate::render::ItemState {
    worst_limit_index(&section.bars)
        .and_then(|index| section.bars.get(index))
        .map_or(crate::render::ItemState::Ok, |bar| {
            crate::render::strip_item_state(
                Some(bar.percent),
                Some((bar.elapsed / 100.0) as f32),
                Some(bar.projected as f32),
                bar.full_at.is_some(),
            )
        })
}

fn status_pill(section: &Section) -> (String, PillState) {
    let dot = "\u{00b7}";
    if section.loading {
        return (format!("{} {dot} Loading", section.title), PillState::Idle);
    }
    if section.empty {
        return (
            format!("{} {dot} No readings", section.title),
            PillState::Idle,
        );
    }
    if section.provider == ProviderKind::Spark {
        if section.error.is_some() {
            return ("Spark \u{00b7} Down".into(), PillState::Crit);
        }
        if let Some(stats) = &section.spark {
            if stats.tok_s > 0.0 {
                return (
                    format!("Spark {dot} {:.0} t/s", stats.tok_s),
                    PillState::Spark,
                );
            }
        }
        return ("Spark \u{00b7} Idle".into(), PillState::Idle);
    }
    if section.stale {
        let age = section.as_of.trim_end_matches(" old");
        return (
            format!("{} {dot} Stale {dot} {age}", section.title),
            PillState::Idle,
        );
    }
    let state = section_state(section);
    let text = worst_limit_index(&section.bars)
        .and_then(|index| section.bars.get(index))
        .and_then(|bar| bar.full_at.as_ref())
        .map_or_else(
            || format!("{} {dot} On pace", section.title),
            |time| format!("{} {dot} Runs out {time}", section.title),
        );
    (
        text,
        match state {
            crate::render::ItemState::Ok => PillState::Good,
            crate::render::ItemState::Warn => PillState::Warn,
            crate::render::ItemState::Crit => PillState::Crit,
        },
    )
}

fn panel_height(section: &Section) -> f32 {
    let mut body = if section.empty {
        58.0
    } else if section.loading {
        let count: usize = if section.provider == ProviderKind::Spark {
            1
        } else {
            2
        };
        count as f32 * 44.0 + count.saturating_sub(1) as f32 * 12.0
    } else if section.provider == ProviderKind::Spark {
        133.0
    } else {
        let count = section.bars.len().max(1);
        count as f32 * 53.0
            + count.saturating_sub(1) as f32 * 12.0
            + if section.provider == ProviderKind::Claude {
                68.0
            } else {
                0.0
            }
    };
    if section.error.is_some()
        && section.provider != ProviderKind::Spark
        && !section.bars.is_empty()
    {
        body += 38.0;
    }
    12.0 + 16.0 + 12.0 + body + 12.0
}

unsafe fn card_layout(snapshot: &Snapshot, width: i32, scale: f32) -> CardLayout {
    let s = |value: f32| (value * scale).round() as i32;
    let content_left = s(13.0);
    let content_right = width - s(13.0);
    let pill_font = make_font(11.0, FW_NORMAL as i32, scale);
    let dc = CreateCompatibleDC(null_mut());
    let old = SelectObject(dc, pill_font);
    let mut pills = Vec::new();
    let mut x = content_left + s(4.0);
    let mut y = s(41.0);
    let pill_right = content_right - s(4.0);
    for section in &snapshot.sections {
        let (text, state) = status_pill(section);
        let width = s(7.0 + 6.0 + 6.0 + 9.0 + 3.0) + text_width(dc, &text);
        if x + width > pill_right && x > content_left + s(4.0) {
            x = content_left + s(4.0);
            y += s(27.0);
        }
        pills.push(PillLayout {
            text,
            state,
            rect: RECT {
                left: x,
                top: y,
                right: (x + width).min(pill_right),
                bottom: y + s(21.0),
            },
        });
        x += width + s(6.0);
    }
    SelectObject(dc, old);
    DeleteObject(pill_font);
    DeleteDC(dc);
    let pill_bottom = pills.iter().map(|pill| pill.rect.bottom).max().unwrap_or(y) + s(4.0);
    let mut panel_y = pill_bottom + s(8.0);
    let mut panels = Vec::new();
    for (index, section) in snapshot.sections.iter().enumerate() {
        let height = (panel_height(section) * scale).round() as i32;
        panels.push(PanelLayout {
            index,
            rect: RECT {
                left: content_left,
                top: panel_y,
                right: content_right,
                bottom: panel_y + height,
            },
        });
        panel_y += height + s(8.0);
    }
    CardLayout {
        height: panel_y - s(8.0) + s(13.0),
        pills,
        panels,
    }
}

#[allow(dead_code)]
fn section_height(section: &Section) -> i32 {
    let mut height = 24 + section.bars.len() as i32 * 55;
    height += match section.provider {
        ProviderKind::Claude => 78,
        ProviderKind::Codex => 20,
        ProviderKind::Spark => 192,
    };
    if section.error.is_some() || section.stale {
        height += 18;
    }
    height
}

fn rounded(x: i32, y: i32, width: i32, height: i32, radius: i32) -> bool {
    if x < 0 || y < 0 || x >= width || y >= height {
        return false;
    }
    let cx = if x < radius {
        radius
    } else if x >= width - radius {
        width - radius - 1
    } else {
        x
    };
    let cy = if y < radius {
        radius
    } else if y >= height - radius {
        height - radius - 1
    } else {
        y
    };
    (x - cx) * (x - cx) + (y - cy) * (y - cy) <= radius * radius
}

unsafe fn fill_rounded(
    dc: HDC,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    radius: i32,
    color: COLORREF,
) {
    let brush = CreateSolidBrush(color);
    let old_brush = SelectObject(dc, brush);
    let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, left, top, right, bottom, radius * 2, radius * 2);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(brush);
}

unsafe fn draw_text_rect(dc: HDC, rect: RECT, value: &str, color: COLORREF, flags: u32) {
    let text = wide(value);
    let mut rect = rect;
    SetTextColor(dc, color);
    DrawTextW(
        dc,
        text.as_ptr(),
        text.len().saturating_sub(1) as i32,
        &mut rect,
        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS | flags,
    );
}

unsafe fn draw_wrapped_text(dc: HDC, rect: RECT, value: &str, color: COLORREF, flags: u32) {
    let text = wide(value);
    let mut rect = rect;
    SetTextColor(dc, color);
    DrawTextW(
        dc,
        text.as_ptr(),
        text.len().saturating_sub(1) as i32,
        &mut rect,
        DT_WORDBREAK | DT_NOPREFIX | flags,
    );
}

fn provider_color(provider: ProviderKind, palette: CardPalette) -> COLORREF {
    match provider {
        ProviderKind::Claude => palette.claude,
        ProviderKind::Codex => palette.codex,
        ProviderKind::Spark => palette.spark,
    }
}

fn item_color(state: crate::render::ItemState, palette: CardPalette) -> COLORREF {
    match state {
        crate::render::ItemState::Ok => palette.text,
        crate::render::ItemState::Warn => palette.warn,
        crate::render::ItemState::Crit => palette.crit,
    }
}

fn fill_color(
    state: crate::render::ItemState,
    provider: ProviderKind,
    palette: CardPalette,
) -> COLORREF {
    match state {
        crate::render::ItemState::Ok => provider_color(provider, palette),
        crate::render::ItemState::Warn => palette.warn,
        crate::render::ItemState::Crit => palette.crit,
    }
}

fn faded(color: COLORREF, background: COLORREF, opacity: f32) -> COLORREF {
    blend_color(background, color, opacity)
}

unsafe fn draw_e2_content(
    pixels: &mut [u32],
    width: i32,
    height: i32,
    snapshot: &Snapshot,
    layout: &CardLayout,
    palette: CardPalette,
    scale: f32,
    pinned: bool,
    hovered: HeaderHit,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let dc = CreateCompatibleDC(null_mut());
    let mut bits = null_mut();
    let info = bitmap_info(width, height);
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
    let old_bitmap = SelectObject(dc, bitmap);
    SetBkMode(dc, TRANSPARENT as i32);

    let f10 = make_font(10.0, FW_NORMAL as i32, scale);
    let f105 = make_font(10.5, FW_NORMAL as i32, scale);
    let f11 = make_font(11.0, FW_NORMAL as i32, scale);
    let f12b = make_font(12.0, FW_SEMIBOLD as i32, scale);
    let f13b = make_font(13.0, FW_SEMIBOLD as i32, scale);
    let f14b = make_font(14.0, FW_SEMIBOLD as i32, scale);
    let f20b = make_display_font(20.0, FW_SEMIBOLD as i32, scale);
    let f30b = make_display_font(30.0, FW_SEMIBOLD as i32, scale);

    let controls = control_rects(width, scale);
    for (hit, rect) in controls {
        if hit == hovered {
            fill_rounded(
                dc,
                rect.left - s(3.0),
                rect.top - s(3.0),
                rect.right + s(3.0),
                rect.bottom + s(3.0),
                s(4.0),
                palette.stale_fill,
            );
        }
    }
    SelectObject(dc, f14b);
    draw_text(dc, s(17.0), s(17.0), "Usage", palette.text);
    let refresh = controls[2].1;
    let pin = controls[1].1;
    let close = controls[0].1;
    draw_shared_glyph(
        dc,
        refresh.left,
        refresh.top,
        s(14.0),
        crate::render::CardGlyph::Refresh,
        palette.muted,
        palette.card,
    );
    draw_shared_glyph(
        dc,
        pin.left,
        pin.top,
        s(14.0),
        if pinned {
            crate::render::CardGlyph::PinFilled
        } else {
            crate::render::CardGlyph::Pin
        },
        palette.muted,
        palette.card,
    );
    draw_shared_glyph(
        dc,
        close.left,
        close.top,
        s(14.0),
        crate::render::CardGlyph::Close,
        palette.muted,
        palette.card,
    );
    SelectObject(dc, f11);
    draw_text_right(
        dc,
        refresh.left - s(10.0),
        s(18.0),
        &snapshot.updated,
        palette.muted,
    );

    for pill in &layout.pills {
        let (base, alpha, dot) = match pill.state {
            PillState::Good => (palette.good, 0.14, palette.good),
            PillState::Warn => (palette.warn, 0.16, palette.warn),
            PillState::Crit => (palette.crit, 0.16, palette.crit),
            PillState::Idle => (palette.stale_fill, 1.0, palette.idle),
            PillState::Spark => (palette.spark, 0.14, palette.spark),
        };
        fill_rounded(
            dc,
            pill.rect.left,
            pill.rect.top,
            pill.rect.right,
            pill.rect.bottom,
            s(10.5),
            if alpha < 1.0 {
                blend_color(palette.card, base, alpha)
            } else {
                base
            },
        );
        let dot_left = pill.rect.left + s(7.0);
        let dot_top = pill.rect.top + (pill.rect.bottom - pill.rect.top - s(6.0)) / 2;
        fill_rounded(
            dc,
            dot_left,
            dot_top,
            dot_left + s(6.0),
            dot_top + s(6.0),
            s(3.0),
            dot,
        );
        SelectObject(dc, f11);
        draw_text_rect(
            dc,
            RECT {
                left: dot_left + s(12.0),
                top: pill.rect.top,
                right: pill.rect.right - s(9.0),
                bottom: pill.rect.bottom,
            },
            &pill.text,
            palette.text,
            DT_LEFT,
        );
    }

    for panel in &layout.panels {
        let section = &snapshot.sections[panel.index];
        draw_e2_panel(
            dc, panel.rect, section, snapshot, palette, scale, f10, f105, f11, f12b, f13b, f20b,
            f30b,
        );
    }

    let mut read_info = info;
    GetDIBits(
        dc,
        bitmap,
        0,
        height as u32,
        pixels.as_mut_ptr().cast(),
        &mut read_info,
        DIB_RGB_COLORS,
    );
    SelectObject(dc, old_bitmap);
    for font in [f10, f105, f11, f12b, f13b, f14b, f20b, f30b] {
        DeleteObject(font);
    }
    DeleteObject(bitmap);
    DeleteDC(dc);
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_e2_panel(
    dc: HDC,
    rect: RECT,
    section: &Section,
    snapshot: &Snapshot,
    palette: CardPalette,
    scale: f32,
    f10: HFONT,
    f105: HFONT,
    f11: HFONT,
    f12b: HFONT,
    f13b: HFONT,
    f20b: HFONT,
    f30b: HFONT,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let opacity = if section.stale { 0.5 } else { 1.0 };
    let panel_fill = faded(palette.panel, palette.card, opacity);
    let panel_border = faded(palette.panel_border, palette.card, opacity);
    fill_rounded(
        dc,
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
        s(8.0),
        panel_border,
    );
    fill_rounded(
        dc,
        rect.left + s(1.0),
        rect.top + s(1.0),
        rect.right - s(1.0),
        rect.bottom - s(1.0),
        s(7.0),
        panel_fill,
    );
    let left = rect.left + s(13.0);
    let right = rect.right - s(13.0);
    let header_y = rect.top + s(12.0);
    let accent = provider_color(section.provider, palette);
    draw_provider_mark(
        dc,
        left,
        header_y + s(1.0),
        s(14.0),
        section.provider,
        faded(accent, palette.card, opacity),
        panel_fill,
    );
    SelectObject(dc, f13b);
    draw_text(
        dc,
        left + s(21.0),
        header_y,
        &section.title,
        faded(palette.text, palette.card, opacity),
    );
    let meta = if section.loading {
        "Loading".to_owned()
    } else {
        match section.provider {
            ProviderKind::Claude => section.today.clone(),
            ProviderKind::Codex => [
                section.chip.as_str(),
                section.today.as_str(),
                section.as_of.as_str(),
            ]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" \u{00b7} "),
            ProviderKind::Spark => section.chip.clone(),
        }
    };
    SelectObject(dc, f105);
    draw_text_rect(
        dc,
        RECT {
            left: left + s(92.0),
            top: header_y,
            right,
            bottom: header_y + s(16.0),
        },
        &meta,
        faded(
            if section.loading {
                palette.faint
            } else {
                palette.muted
            },
            palette.card,
            opacity,
        ),
        DT_RIGHT,
    );
    let mut body_y = header_y + s(28.0);

    if section.empty {
        SelectObject(dc, f12b);
        draw_text_rect(
            dc,
            RECT {
                left,
                top: body_y + s(8.0),
                right,
                bottom: body_y + s(24.0),
            },
            "No readings yet",
            faded(palette.text, palette.card, opacity),
            DT_CENTER,
        );
        SelectObject(dc, f11);
        draw_wrapped_text(
            dc,
            RECT {
                left,
                top: body_y + s(25.0),
                right,
                bottom: body_y + s(55.0),
            },
            "The first reading appears here after the next refresh.",
            faded(palette.muted, palette.card, opacity),
            DT_CENTER,
        );
        return;
    }
    if section.loading {
        let count = if section.provider == ProviderKind::Spark {
            1
        } else {
            2
        };
        let skeleton = blend_color(
            panel_fill,
            palette.track,
            snapshot.pulse_alpha.clamp(0.4, 1.0),
        );
        for _ in 0..count {
            fill_rounded(
                dc,
                left,
                body_y,
                left + s(40.0),
                body_y + s(18.0),
                s(4.0),
                skeleton,
            );
            fill_rounded(
                dc,
                left + s(46.0),
                body_y + s(5.0),
                left + s(136.0),
                body_y + s(13.0),
                s(4.0),
                skeleton,
            );
            fill_rounded(
                dc,
                left,
                body_y + s(30.0),
                right,
                body_y + s(36.0),
                s(1.5),
                skeleton,
            );
            body_y += s(56.0);
        }
        return;
    }

    if section.provider == ProviderKind::Spark {
        draw_e2_spark(
            dc,
            left,
            right,
            body_y,
            section,
            palette,
            scale,
            f10,
            f11,
            f13b,
            f30b,
            panel_fill,
            snapshot.pulse_alpha,
        );
        return;
    }

    if let Some(error) = &section.error {
        draw_shared_glyph(
            dc,
            left,
            body_y,
            s(12.0),
            crate::render::CardGlyph::Warning,
            palette.warn,
            panel_fill,
        );
        SelectObject(dc, f11);
        draw_wrapped_text(
            dc,
            RECT {
                left: left + s(19.0),
                top: body_y - s(1.0),
                right,
                bottom: body_y + s(30.0),
            },
            error,
            palette.muted,
            DT_LEFT,
        );
        body_y += s(38.0);
    }
    let limit_opacity = if section.error.is_some() {
        0.5
    } else {
        opacity
    };
    for (index, bar) in section.bars.iter().enumerate() {
        draw_limit_block(
            dc,
            left,
            right,
            body_y,
            section.provider,
            bar,
            snapshot.show_left,
            palette,
            panel_fill,
            limit_opacity,
            scale,
            f105,
            f11,
            f20b,
        );
        body_y += s(53.0);
        if index + 1 < section.bars.len() {
            body_y += s(12.0);
        }
    }
    if section.provider == ProviderKind::Claude {
        body_y += s(12.0);
        draw_line(dc, left, body_y, right, body_y, palette.separator, s(1.0));
        body_y += s(10.0);
        SelectObject(dc, f10);
        draw_text(dc, left, body_y, "5-hour \u{00b7} last 24 h", palette.faint);
        draw_text_right(dc, right, body_y, "now", palette.faint);
        body_y += s(16.0);
        draw_e2_chart(
            dc,
            &section.chart,
            RECT {
                left,
                top: body_y,
                right,
                bottom: body_y + s(30.0),
            },
            palette.claude,
            palette.separator,
            panel_fill,
            false,
            true,
            1.0,
            scale,
        );
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_limit_block(
    dc: HDC,
    left: i32,
    right: i32,
    top: i32,
    provider: ProviderKind,
    bar: &Bar,
    show_left: bool,
    palette: CardPalette,
    background: COLORREF,
    opacity: f32,
    scale: f32,
    f105: HFONT,
    f11: HFONT,
    f20b: HFONT,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let state = crate::render::strip_item_state(
        Some(bar.percent),
        Some((bar.elapsed / 100.0) as f32),
        Some(bar.projected as f32),
        bar.full_at.is_some(),
    );
    let value_color = faded(item_color(state, palette), background, opacity);
    let used = bar.percent.round().clamp(0.0, 100.0) as i32;
    let meter_used = bar.meter_percent.unwrap_or(bar.percent);
    let shown = if show_left { 100 - used } else { used };
    let number = format!("{shown}%");
    SelectObject(dc, f20b);
    if let Some(previous) = bar.previous_percent {
        let previous = previous.round().clamp(0.0, 100.0) as i32;
        let previous = if show_left { 100 - previous } else { previous };
        draw_text_rect(
            dc,
            RECT {
                left,
                top,
                right: left + s(64.0),
                bottom: top + s(22.0),
            },
            &format!("{previous}%"),
            faded(value_color, background, 1.0 - bar.number_progress),
            DT_LEFT,
        );
    }
    draw_text_rect(
        dc,
        RECT {
            left,
            top,
            right: left + s(64.0),
            bottom: top + s(22.0),
        },
        &number,
        if bar.previous_percent.is_some() {
            faded(value_color, background, bar.number_progress)
        } else {
            value_color
        },
        DT_LEFT,
    );
    let number_width = text_width(dc, &number);
    SelectObject(dc, f11);
    let label = if show_left {
        format!("left \u{00b7} {}", bar.label)
    } else {
        format!("{} \u{00b7} {}% left", bar.label, 100 - used)
    };
    SelectObject(dc, f105);
    let reset = if bar.reset.is_empty() {
        String::new()
    } else {
        format!("resets {}", bar.reset)
    };
    let reset_width = text_width(dc, &reset);
    draw_text_rect(
        dc,
        RECT {
            left: left + number_width + s(6.0),
            top: top + s(3.0),
            right: (right - reset_width - s(8.0)).max(left + number_width + s(8.0)),
            bottom: top + s(22.0),
        },
        &label,
        faded(palette.muted, background, opacity),
        DT_LEFT,
    );
    draw_text_rect(
        dc,
        RECT {
            left: right - reset_width,
            top: top + s(3.0),
            right,
            bottom: top + s(22.0),
        },
        &reset,
        faded(palette.faint, background, opacity),
        DT_RIGHT,
    );

    let meter_top = top + s(28.0);
    let gap = s(2.0);
    let total = right - left;
    let fill = fill_color(state, provider, palette);
    for index in 0..20 {
        let segment_left = left + ((index as f32 * (total + gap) as f32 / 20.0).round() as i32);
        let segment_right = if index == 19 {
            right
        } else {
            left + (((index + 1) as f32 * (total + gap) as f32 / 20.0).round() as i32) - gap
        };
        let band = crate::render::segment_band(
            index,
            meter_used as f32,
            bar.projected as f32,
            bar.elapsed as f32,
        );
        let color = match band {
            crate::render::SegmentBand::Used => fill,
            crate::render::SegmentBand::Projected => blend_color(background, fill, 0.33),
            crate::render::SegmentBand::Elapsed => palette.elapsed,
            crate::render::SegmentBand::Track => palette.track,
        };
        fill_rounded(
            dc,
            segment_left,
            meter_top,
            segment_right.max(segment_left + 1),
            meter_top + s(6.0),
            s(1.5),
            faded(color, background, opacity),
        );
    }
    let ahead = bar.ahead_by.round() as i32;
    let (copy, color) = if ahead > 0 {
        let ending = bar
            .full_at
            .as_ref()
            .map(|time| format!("empty ~{time}"))
            .unwrap_or_else(|| format!("ends near {:.0}%", bar.projected));
        (
            format!("\u{25B2} {ahead} ahead of pace \u{00b7} {ending}"),
            if bar.full_at.is_some() {
                palette.crit
            } else {
                palette.warn
            },
        )
    } else {
        (
            format!(
                "\u{25BC} {} under pace \u{00b7} ends near {:.0}%",
                -ahead, bar.projected
            ),
            palette.good,
        )
    };
    SelectObject(dc, f105);
    draw_text_rect(
        dc,
        RECT {
            left,
            top: top + s(40.0),
            right,
            bottom: top + s(53.0),
        },
        &copy,
        faded(color, background, opacity),
        DT_LEFT,
    );
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_e2_spark(
    dc: HDC,
    left: i32,
    right: i32,
    top: i32,
    section: &Section,
    palette: CardPalette,
    scale: f32,
    f10: HFONT,
    f11: HFONT,
    f13b: HFONT,
    f30b: HFONT,
    background: COLORREF,
    pulse_alpha: f32,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let Some(stats) = &section.spark else { return };
    let down = section.error.is_some();
    let value = if down {
        "down".to_owned()
    } else {
        format!("{:.0}", stats.tok_s)
    };
    let show_running = stats.running > 0 && right - left >= s(420.0);
    let hero_width = s(if show_running { 190.0 } else { 116.0 });
    SelectObject(dc, f30b);
    draw_text_rect(
        dc,
        RECT {
            left,
            top,
            right: left + s(76.0),
            bottom: top + s(32.0),
        },
        &value,
        if down { palette.crit } else { palette.text },
        DT_LEFT,
    );
    SelectObject(dc, f11);
    let sub = if down {
        "unreachable \u{00b7} 4 m".to_owned()
    } else {
        let running = if show_running {
            format!(" \u{00b7} {} running", stats.running)
        } else {
            String::new()
        };
        format!("tok/s \u{00b7} peak {:.0}{running}", stats.peak)
    };
    draw_text_rect(
        dc,
        RECT {
            left,
            top: top + s(31.0),
            right: left + hero_width,
            bottom: top + s(45.0),
        },
        &sub,
        palette.muted,
        DT_LEFT,
    );
    let chart_left = (left + hero_width + s(4.0)).min(right - s(50.0));
    draw_e2_chart(
        dc,
        &section.chart,
        RECT {
            left: chart_left,
            top,
            right,
            bottom: top + s(38.0),
        },
        if down { palette.faint } else { palette.spark },
        palette.separator,
        background,
        true,
        !down,
        pulse_alpha,
        scale,
    );
    let stats_top = top + s(50.0);
    draw_line(
        dc,
        left,
        stats_top,
        right,
        stats_top,
        palette.separator,
        s(1.0),
    );
    let row_top = stats_top + s(9.0);
    let gap = s(8.0);
    let column_width = (right - left - gap * 3) / 4;
    let columns = [
        ("KV cache", percent(stats.kv_pct), String::new()),
        ("TTFT", unit(stats.ttft_ms, "ms"), String::new()),
        (
            "GPU",
            format!("{:.0}%", stats.gpu_util),
            match (stats.gpu_temp, stats.gpu_power) {
                (Some(temp), Some(power)) => format!("{temp:.0} \u{00b0}C \u{00b7} {power:.0} W"),
                _ => String::new(),
            },
        ),
        (
            "Memory",
            percent(stats.mem_pct),
            stats
                .swap_pct
                .map_or_else(String::new, |value| format!("swap {value:.0}%")),
        ),
    ];
    for (index, (label, value, sub)) in columns.iter().enumerate() {
        let x = left + index as i32 * (column_width + gap);
        SelectObject(dc, f10);
        draw_text_rect(
            dc,
            RECT {
                left: x,
                top: row_top,
                right: x + column_width,
                bottom: row_top + s(12.0),
            },
            label,
            palette.muted,
            DT_LEFT,
        );
        SelectObject(dc, f13b);
        draw_text_rect(
            dc,
            RECT {
                left: x,
                top: row_top + s(13.0),
                right: x + column_width,
                bottom: row_top + s(29.0),
            },
            if down { "\u{2014}" } else { value },
            if down { palette.faint } else { palette.text },
            DT_LEFT,
        );
        if !down {
            SelectObject(dc, f10);
            draw_text_rect(
                dc,
                RECT {
                    left: x,
                    top: row_top + s(29.0),
                    right: x + column_width,
                    bottom: row_top + s(41.0),
                },
                sub,
                palette.faint,
                DT_LEFT,
            );
        }
    }
    let chips_y = top + s(114.0);
    let mut x = left;
    SelectObject(dc, f11);
    for service in &stats.aux {
        let text = format!(
            "{} {}",
            service.name,
            if down {
                "down"
            } else if service.busy {
                "busy"
            } else {
                "idle"
            }
        );
        let width = s(9.0 + 6.0 + 5.0 + 9.0) + text_width(dc, &text);
        if x + width > right {
            break;
        }
        fill_rounded(
            dc,
            x,
            chips_y,
            x + width,
            chips_y + s(19.0),
            s(9.5),
            palette.chip,
        );
        let dot = if down {
            palette.crit
        } else if service.busy {
            palette.spark
        } else {
            palette.idle
        };
        fill_rounded(
            dc,
            x + s(9.0),
            chips_y + s(6.5),
            x + s(15.0),
            chips_y + s(12.5),
            s(3.0),
            dot,
        );
        draw_text_rect(
            dc,
            RECT {
                left: x + s(20.0),
                top: chips_y,
                right: x + width - s(9.0),
                bottom: chips_y + s(19.0),
            },
            &text,
            palette.muted,
            DT_LEFT,
        );
        x += width + s(6.0);
    }
}

unsafe fn draw_e2_chart(
    dc: HDC,
    values: &[f64],
    rect: RECT,
    accent: COLORREF,
    grid: COLORREF,
    background: COLORREF,
    spark_chart: bool,
    show_dot: bool,
    pulse_alpha: f32,
    scale: f32,
) {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if !spark_chart {
        for fraction in [0.0, 0.5] {
            let y = rect.top + (height as f32 * fraction).round() as i32;
            let mut x = rect.left;
            while x < rect.right {
                draw_line(dc, x, y, (x + 2).min(rect.right), y, grid, 1);
                x += 5;
            }
        }
        for fraction in [0.25, 0.5, 0.75] {
            let x = rect.left + (width as f32 * fraction).round() as i32;
            let mut y = rect.top;
            while y < rect.bottom {
                draw_line(dc, x, y, x, (y + 2).min(rect.bottom), grid, 1);
                y += 5;
            }
        }
        draw_line(
            dc,
            rect.left,
            rect.bottom - 1,
            rect.right,
            rect.bottom - 1,
            grid,
            1,
        );
    }
    if values.len() < 2 {
        return;
    }
    let max = if spark_chart {
        values.iter().copied().fold(1.0, f64::max)
    } else {
        100.0
    };
    let points = values
        .iter()
        .enumerate()
        .map(|(index, value)| POINT {
            x: rect.left
                + (index as f32 * (width - 1) as f32 / (values.len() - 1) as f32).round() as i32,
            y: rect.bottom - 1 - (value.clamp(0.0, max) / max * (height - 2) as f64).round() as i32,
        })
        .collect::<Vec<_>>();
    if !spark_chart || show_dot {
        for x in rect.left..rect.right {
            let position =
                (x - rect.left) as f32 * (points.len() - 1) as f32 / (width - 1).max(1) as f32;
            let index = position.floor() as usize;
            let t = position - index as f32;
            let a = points[index.min(points.len() - 1)];
            let b = points[(index + 1).min(points.len() - 1)];
            let y = (a.y as f32 + (b.y - a.y) as f32 * t).round() as i32;
            for py in y..rect.bottom {
                let depth = (py - y) as f32 / (rect.bottom - y).max(1) as f32;
                SetPixelV(
                    dc,
                    x,
                    py,
                    blend_color(background, accent, 0.30 * (1.0 - depth) + 0.02 * depth),
                );
            }
        }
    }
    let pen = CreatePen(PS_SOLID, (1.5 * scale).round().max(1.0) as i32, accent);
    let old_pen = SelectObject(dc, pen);
    Polyline(dc, points.as_ptr(), points.len() as i32);
    SelectObject(dc, old_pen);
    DeleteObject(pen);
    let point = *points.last().unwrap();
    if spark_chart && show_dot {
        let halo_color = blend_color(background, accent, 0.10 + 0.12 * pulse_alpha);
        fill_rounded(
            dc,
            point.x - (4.4 * scale).round() as i32,
            point.y - (4.4 * scale).round() as i32,
            point.x + (4.4 * scale).round() as i32,
            point.y + (4.4 * scale).round() as i32,
            (4.4 * scale).round() as i32,
            halo_color,
        );
    }
    if show_dot {
        fill_rounded(
            dc,
            point.x - (2.0 * scale).round() as i32,
            point.y - (2.0 * scale).round() as i32,
            point.x + (2.0 * scale).round() as i32,
            point.y + (2.0 * scale).round() as i32,
            (2.0 * scale).round() as i32,
            accent,
        );
    }
}

#[allow(dead_code)]
unsafe fn draw_content(
    pixels: &mut [u32],
    width: i32,
    height: i32,
    snapshot: &Snapshot,
    dark: bool,
    scale: f32,
    pinned: bool,
    hovered: HeaderHit,
) {
    let dc = CreateCompatibleDC(null_mut());
    let mut bits = null_mut();
    let info = bitmap_info(width, height);
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
    let old_bitmap = SelectObject(dc, bitmap);
    SetBkMode(dc, TRANSPARENT as i32);

    let normal = make_font(12.0, FW_NORMAL as i32, scale);
    let small = make_font(10.5, FW_NORMAL as i32, scale);
    let semibold = make_font(13.0, FW_SEMIBOLD as i32, scale);
    let header = make_font(14.0, FW_SEMIBOLD as i32, scale);
    let big = make_font(17.0, FW_SEMIBOLD as i32, scale);
    let text = if dark {
        rgb(235, 235, 238)
    } else {
        rgb(30, 30, 34)
    };
    let muted = if dark {
        rgb(164, 164, 172)
    } else {
        rgb(95, 95, 104)
    };
    let track = if dark {
        rgb(66, 66, 72)
    } else {
        rgb(224, 224, 229)
    };
    let separator = if dark {
        rgb(55, 55, 59)
    } else {
        rgb(220, 220, 224)
    };
    let tile = if dark {
        rgb(42, 42, 46)
    } else {
        rgb(238, 238, 241)
    };
    let card_bg = if dark {
        rgb(32, 32, 34)
    } else {
        rgb(248, 248, 250)
    };
    let green = rgb(118, 185, 0);
    let dim_green = if dark {
        rgb(126, 174, 91)
    } else {
        rgb(65, 120, 28)
    };
    let amber = rgb(224, 148, 55);
    let red = if dark {
        rgb(255, 123, 114)
    } else {
        rgb(196, 43, 28)
    };
    let pad = (16.0 * scale) as i32;
    let mut y = (14.0 * scale) as i32;

    let controls = control_rects(width, scale);
    for (hit, rect) in controls {
        if hit == hovered {
            fill_round(
                dc,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                tile,
            );
        }
    }

    SelectObject(dc, header);
    draw_text(dc, pad, y, "Usage", text);
    SelectObject(dc, small);
    let controls_left = controls[2].1.left;
    draw_text_right(dc, controls_left - 6, y + 2, &snapshot.updated, muted);
    let refresh = controls[2].1;
    draw_shared_glyph(
        dc,
        refresh.left + (4.0 * scale) as i32,
        refresh.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        crate::render::CardGlyph::Refresh,
        muted,
        card_bg,
    );
    let pin = controls[1].1;
    draw_shared_glyph(
        dc,
        pin.left + (4.0 * scale) as i32,
        pin.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        if pinned {
            crate::render::CardGlyph::PinFilled
        } else {
            crate::render::CardGlyph::Pin
        },
        muted,
        card_bg,
    );
    let close = controls[0].1;
    draw_shared_glyph(
        dc,
        close.left + (4.0 * scale) as i32,
        close.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        crate::render::CardGlyph::Close,
        muted,
        card_bg,
    );
    y += (30.0 * scale) as i32;

    for (index, section) in snapshot.sections.iter().enumerate() {
        if index > 0 {
            let line_y = y + (6.0 * scale) as i32;
            draw_line(dc, pad, line_y, width - pad, line_y, separator, 1);
            y += (15.0 * scale) as i32;
        }
        let accent = provider_accent(section.provider);
        let dimmed = section.stale || section.error.is_some();
        draw_provider_mark(
            dc,
            pad,
            y + 1,
            (14.0 * scale) as i32,
            section.provider,
            accent,
            card_bg,
        );
        SelectObject(dc, semibold);
        draw_text(
            dc,
            pad + (20.0 * scale) as i32,
            y,
            &section.title,
            if dimmed { muted } else { text },
        );
        if !section.chip.is_empty() {
            SelectObject(dc, small);
            draw_chip(dc, width - pad, y - 1, &section.chip, scale, tile, muted);
        }
        y += (24.0 * scale) as i32;

        for bar in &section.bars {
            SelectObject(dc, normal);
            draw_text(dc, pad, y, &bar.label, muted);
            let reset = if bar.reset.is_empty() {
                String::new()
            } else {
                format!(" · resets {}", bar.reset)
            };
            draw_text_right(dc, width - pad, y, &reset, muted);
            let reset_width = text_width(dc, &reset);
            SelectObject(dc, semibold);
            draw_text_right(
                dc,
                width - pad - reset_width,
                y,
                &format!("{:.0}%", bar.percent),
                if dimmed { muted } else { text },
            );

            let bar_top = y + (20.0 * scale) as i32;
            let bar_height = (6.0 * scale).round() as i32;
            let bar_width = width - 2 * pad;
            fill_round(dc, pad, bar_top, bar_width, bar_height, track);
            let fill_width =
                ((bar_width as f64 * bar.percent.clamp(0.0, 100.0) / 100.0).round() as i32).max(2);
            let fill = if section.error.is_some() {
                muted
            } else if section.stale {
                blend_color(track, accent, 0.58)
            } else if bar.percent > 95.0 {
                red
            } else if bar.percent > 75.0 {
                amber
            } else {
                accent
            };
            fill_round(dc, pad, bar_top, fill_width, bar_height, fill);
            let tick_x = pad + (bar_width as f64 * bar.elapsed.clamp(0.0, 100.0) / 100.0) as i32;
            draw_line(
                dc,
                tick_x,
                bar_top - (3.0 * scale) as i32,
                tick_x,
                bar_top + bar_height + (3.0 * scale) as i32,
                text,
                (2.0 * scale).round() as i32,
            );

            SelectObject(dc, small);
            let ahead = bar.ahead_by.round() as i64;
            let (arrow, wording, color) = if ahead > 0 {
                let ending = bar
                    .full_at
                    .as_ref()
                    .map(|at| format!("empty ~{at}"))
                    .unwrap_or_else(|| format!("ends near {:.0}%", bar.projected));
                (
                    "▲",
                    format!("{ahead} ahead of pace · {ending}"),
                    if bar.full_at.is_some() { red } else { amber },
                )
            } else {
                (
                    "▼",
                    format!("{} under pace · ends near {:.0}%", -ahead, bar.projected),
                    dim_green,
                )
            };
            let forecast_y = bar_top + bar_height + (7.0 * scale) as i32;
            draw_text(
                dc,
                pad,
                forecast_y,
                arrow,
                if dimmed { muted } else { color },
            );
            draw_text(
                dc,
                pad + (13.0 * scale) as i32,
                forecast_y,
                &wording,
                if dimmed { muted } else { color },
            );
            y += (55.0 * scale) as i32;
        }

        match section.provider {
            ProviderKind::Claude => {
                draw_area_chart(
                    dc,
                    &section.chart,
                    pad,
                    y,
                    width - pad,
                    y + (40.0 * scale) as i32,
                    accent,
                    tile,
                    separator,
                    true,
                );
                SelectObject(dc, small);
                draw_text(dc, pad, y + (42.0 * scale) as i32, "24 h", muted);
                draw_text_right(dc, width - pad, y + (42.0 * scale) as i32, "now", muted);
                y += (58.0 * scale) as i32;
                SelectObject(dc, normal);
                draw_text(
                    dc,
                    pad,
                    y,
                    &section.today,
                    if dimmed { muted } else { text },
                );
                y += (20.0 * scale) as i32;
            }
            ProviderKind::Codex => {
                SelectObject(dc, normal);
                draw_text(
                    dc,
                    pad,
                    y,
                    &section.today,
                    if dimmed { muted } else { text },
                );
                draw_text_right(dc, width - pad, y, &section.as_of, muted);
                y += (20.0 * scale) as i32;
            }
            ProviderKind::Spark => {
                if let Some(stats) = &section.spark {
                    draw_stat_grid(
                        dc,
                        stats,
                        pad,
                        y,
                        width - pad,
                        scale,
                        tile,
                        text,
                        muted,
                        big,
                        small,
                    );
                    y += (106.0 * scale) as i32;
                    draw_area_chart(
                        dc,
                        &section.chart,
                        pad,
                        y,
                        width - pad,
                        y + (44.0 * scale) as i32,
                        accent,
                        tile,
                        separator,
                        false,
                    );
                    y += (58.0 * scale) as i32;
                    draw_aux(
                        dc,
                        &stats.aux,
                        pad,
                        y,
                        width - pad,
                        scale,
                        tile,
                        text,
                        green,
                        red,
                        small,
                    );
                    y += (28.0 * scale) as i32;
                }
            }
        }
        if let Some(error) = &section.error {
            SelectObject(dc, small);
            draw_text(dc, pad, y, &format!("⚠ {error}"), muted);
            y += (18.0 * scale) as i32;
        } else if section.stale {
            SelectObject(dc, small);
            draw_text(dc, pad, y, "⚠ Data is stale", muted);
            y += (18.0 * scale) as i32;
        }
    }

    let mut read_info = info;
    GetDIBits(
        dc,
        bitmap,
        0,
        height as u32,
        pixels.as_mut_ptr().cast(),
        &mut read_info,
        DIB_RGB_COLORS,
    );
    for y in 0..height {
        for x in 0..width {
            if rounded(x, y, width, height, (10.0 * scale) as i32) {
                pixels[(y * width + x) as usize] |= 247u32 << 24;
            }
        }
    }
    SelectObject(dc, old_bitmap);
    for font in [normal, small, semibold, header, big] {
        DeleteObject(font);
    }
    DeleteObject(bitmap);
    DeleteDC(dc);
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        },
        ..unsafe { std::mem::zeroed() }
    }
}

unsafe fn make_font(size: f32, weight: i32, scale: f32) -> HFONT {
    CreateFontW(
        -(size * scale).round() as i32,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Segoe UI Variable Text").as_ptr(),
    )
}

unsafe fn make_display_font(size: f32, weight: i32, scale: f32) -> HFONT {
    CreateFontW(
        -(size * scale).round() as i32,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Segoe UI Variable Display").as_ptr(),
    )
}
unsafe fn draw_text(dc: HDC, x: i32, y: i32, value: &str, color: COLORREF) {
    let v = wide(value);
    SetTextColor(dc, color);
    TextOutW(dc, x, y, v.as_ptr(), v.len().saturating_sub(1) as i32);
}
unsafe fn text_width(dc: HDC, value: &str) -> i32 {
    let v = wide(value);
    let mut s = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(dc, v.as_ptr(), v.len().saturating_sub(1) as i32, &mut s);
    s.cx
}
unsafe fn draw_text_right(dc: HDC, right: i32, y: i32, value: &str, color: COLORREF) {
    draw_text(dc, right - text_width(dc, value), y, value, color);
}
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

unsafe fn draw_line(dc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, color: COLORREF, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), color);
    let old = SelectObject(dc, pen);
    MoveToEx(dc, x1, y1, null_mut());
    LineTo(dc, x2, y2);
    SelectObject(dc, old);
    DeleteObject(pen);
}
unsafe fn fill_round(dc: HDC, x: i32, y: i32, width: i32, height: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, x, y, x + width, y + height, height, height);
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(brush);
}
fn provider_accent(provider: ProviderKind) -> COLORREF {
    match provider {
        ProviderKind::Claude => rgb(217, 119, 87),
        ProviderKind::Codex => rgb(76, 154, 255),
        ProviderKind::Spark => rgb(118, 185, 0),
    }
}
unsafe fn draw_chip(
    dc: HDC,
    right: i32,
    y: i32,
    value: &str,
    scale: f32,
    fill: COLORREF,
    color: COLORREF,
) {
    let width = text_width(dc, value) + (12.0 * scale) as i32;
    let height = (18.0 * scale) as i32;
    fill_round(dc, right - width, y, width, height, fill);
    draw_text(
        dc,
        right - width + (6.0 * scale) as i32,
        y + 1,
        value,
        color,
    );
}

unsafe fn draw_provider_mark(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    provider: ProviderKind,
    color: COLORREF,
    background: COLORREF,
) {
    let mask = match provider {
        ProviderKind::Claude => crate::render::draw_mark(size),
        ProviderKind::Codex => crate::render::draw_codex_mark(size),
        ProviderKind::Spark => crate::render::draw_spark_mark(size),
    };
    draw_mask(dc, x, y, size, &mask, color, background);
}

unsafe fn draw_mask(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    mask: &[f32],
    color: COLORREF,
    background: COLORREF,
) {
    for py in 0..size {
        for px in 0..size {
            let coverage = mask[(py * size + px) as usize];
            if coverage > 0.0 {
                SetPixelV(dc, x + px, y + py, blend_color(background, color, coverage));
            }
        }
    }
}

unsafe fn draw_shared_glyph(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    glyph: crate::render::CardGlyph,
    color: COLORREF,
    background: COLORREF,
) {
    let mask = crate::render::draw_card_glyph(size, glyph);
    draw_mask(dc, x, y, size, &mask, color, background);
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_stat_grid(
    dc: HDC,
    s: &SparkStats,
    left: i32,
    top: i32,
    right: i32,
    scale: f32,
    tile: COLORREF,
    text: COLORREF,
    muted: COLORREF,
    big: HFONT,
    small: HFONT,
) {
    let gap = (6.0 * scale) as i32;
    let width = (right - left - 2 * gap) / 3;
    let height = (48.0 * scale) as i32;
    let v = [
        (
            "tok/s",
            format!("{:.0}", s.tok_s),
            format!("peak {:.0}", s.peak),
        ),
        (
            "running / waiting",
            format!("{} / {}", s.running, s.waiting),
            String::new(),
        ),
        ("KV", percent(s.kv_pct), String::new()),
        ("TTFT", unit(s.ttft_ms, "ms"), String::new()),
        (
            "GPU",
            format!("{:.0}%", s.gpu_util),
            format!("{} C · {} W", number(s.gpu_temp), number(s.gpu_power)),
        ),
        (
            "Memory",
            percent(s.mem_pct),
            format!("swap {} %", number(s.swap_pct)),
        ),
    ];
    for (i, (label, value, detail)) in v.iter().enumerate() {
        let col = i % 3;
        let row = i / 3;
        let x = left + col as i32 * (width + gap);
        let y = top + row as i32 * (height + gap);
        fill_round(dc, x, y, width, height, tile);
        SelectObject(dc, small);
        draw_text(dc, x + (7.0 * scale) as i32, y + 4, label, muted);
        SelectObject(dc, big);
        draw_text(
            dc,
            x + (7.0 * scale) as i32,
            y + (17.0 * scale) as i32,
            value,
            text,
        );
        if !detail.is_empty() {
            SelectObject(dc, small);
            let detail_y = if i == 0 {
                y + 4
            } else {
                y + (33.0 * scale) as i32
            };
            draw_text_right(dc, x + width - 6, detail_y, detail, muted);
        }
    }
}
fn number(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.0}")).unwrap_or_else(|| "-".into())
}
fn percent(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.0}%")).unwrap_or_else(|| "-".into())
}
fn unit(v: Option<f64>, suffix: &str) -> String {
    v.map(|n| format!("{n:.0} {suffix}"))
        .unwrap_or_else(|| "-".into())
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_aux(
    dc: HDC,
    aux: &[AuxStatus],
    left: i32,
    top: i32,
    right: i32,
    scale: f32,
    fill: COLORREF,
    text: COLORREF,
    green: COLORREF,
    red: COLORREF,
    font: HFONT,
) {
    SelectObject(dc, font);
    let mut x = left;
    for service in aux {
        let state = if service.busy {
            "busy"
        } else if service.up {
            "idle"
        } else {
            "down"
        };
        let label = format!("● {} {state}", service.name);
        let width = text_width(dc, &label) + (12.0 * scale) as i32;
        if x + width > right {
            break;
        }
        fill_round(dc, x, top, width, (20.0 * scale) as i32, fill);
        let color = if service.busy {
            green
        } else if service.up {
            text
        } else {
            red
        };
        draw_text(dc, x + (6.0 * scale) as i32, top + 2, &label, color);
        x += width + (6.0 * scale) as i32;
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_area_chart(
    dc: HDC,
    values: &[f64],
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    color: COLORREF,
    background: COLORREF,
    grid: COLORREF,
    percent_scale: bool,
) {
    for step in 0..=2 {
        let y = top + (bottom - top) * step / 2;
        draw_line(dc, left, y, right, y, grid, 1);
    }
    for step in 0..=4 {
        let x = left + (right - left) * step / 4;
        draw_line(dc, x, top, x, bottom, grid, 1);
    }
    if values.len() < 3 {
        return;
    }
    let peak = if percent_scale {
        100.0
    } else {
        values.iter().copied().fold(1.0, f64::max)
    };
    let mut points = Vec::with_capacity(values.len() + 2);
    points.push(POINT { x: left, y: bottom });
    for (index, value) in values.iter().enumerate() {
        points.push(POINT {
            x: left + (right - left) * index as i32 / (values.len() - 1) as i32,
            y: bottom - ((bottom - top) as f64 * value.max(0.0) / peak) as i32,
        });
    }
    points.push(POINT {
        x: right,
        y: bottom,
    });
    let brush = CreateSolidBrush(blend_color(background, color, 0.25));
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    Polygon(dc, points.as_ptr(), points.len() as i32);
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(brush);
    let line = &points[1..points.len() - 1];
    let pen = CreatePen(PS_SOLID, 2, color);
    let old = SelectObject(dc, pen);
    Polyline(dc, line.as_ptr(), line.len() as i32);
    SelectObject(dc, old);
    DeleteObject(pen);
}
fn blend_color(bg: COLORREF, fg: COLORREF, a: f32) -> COLORREF {
    let c = |s| {
        let b = ((bg >> s) & 255u32) as f32;
        let f = ((fg >> s) & 255u32) as f32;
        (b * (1.0 - a) + f * a).round() as u32
    };
    c(0) | (c(8) << 8) | (c(16) << 16)
}

unsafe fn present(hwnd: HWND, point: POINT, frame: (i32, i32, Vec<u32>), alpha: u8) {
    let (width, height, pixels) = frame;
    let screen = GetDC(null_mut());
    let dc = CreateCompatibleDC(screen);
    let mut bits = null_mut();
    let info = bitmap_info(width, height);
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
    let old = SelectObject(dc, bitmap);
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(
        hwnd, screen, &point, &size, dc, &source, 0, &blend, ULW_ALPHA,
    );
    SelectObject(dc, old);
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(null_mut(), screen);
}

unsafe fn redraw(hwnd: HWND) {
    let mut view = VIEW.with(|view| view.borrow().clone());
    let Some(mut view) = view.take() else {
        return;
    };
    let now = Instant::now();
    let open_progress = (now.duration_since(view.opened_at).as_secs_f32() / 0.150).min(1.0);
    let eased = 1.0 - (1.0 - open_progress).powi(3);
    let closing_progress = CLOSING_AT.with(|closing| {
        closing.borrow().map_or(0.0, |started| {
            (now.duration_since(started).as_secs_f32() / 0.100).min(1.0)
        })
    });
    if closing_progress >= 1.0 {
        KillTimer(hwnd, ANIMATION_TIMER);
        ShowWindow(hwnd, SW_HIDE);
        CLOSING_AT.with(|closing| *closing.borrow_mut() = None);
        return;
    }
    let phase = now.duration_since(view.opened_at).as_secs_f32() / 1.2;
    view.snapshot.pulse_alpha = 0.7 - 0.3 * (phase * std::f32::consts::TAU).cos();
    let change_elapsed = now.duration_since(view.changed_at).as_secs_f32();
    if let Some(previous) = &view.previous {
        for (section_index, section) in view.snapshot.sections.iter_mut().enumerate() {
            let Some(old_section) = previous.sections.get(section_index) else {
                continue;
            };
            for (bar_index, bar) in section.bars.iter_mut().enumerate() {
                let Some(old_bar) = old_section.bars.get(bar_index) else {
                    continue;
                };
                let number_t = (change_elapsed / 0.250).min(1.0);
                let meter_t = (change_elapsed / 0.300).min(1.0);
                let number_eased = 1.0 - (1.0 - number_t).powi(3);
                let meter_eased = 1.0 - (1.0 - meter_t).powi(3);
                bar.previous_percent = Some(old_bar.percent);
                bar.number_progress = number_eased;
                bar.meter_percent =
                    Some(old_bar.percent + (bar.percent - old_bar.percent) * meter_eased as f64);
            }
        }
    }
    let frame = render_with_hover(
        &view.snapshot,
        view.dark,
        view.dpi,
        is_pinned(),
        HOVER_HIT.with(Cell::get),
        view.width,
    );
    let slide = ((1.0 - eased) * 8.0 * view.dpi as f32 / 96.0).round() as i32;
    let alpha = (255.0 * eased * (1.0 - closing_progress)).round() as u8;
    present(
        hwnd,
        POINT {
            x: view.target.x,
            y: view.target.y + slide,
        },
        frame,
        alpha,
    );
    let animating_content = view.snapshot.sections.iter().any(|section| {
        section.loading
            || (section.provider == ProviderKind::Spark
                && section.error.is_none()
                && section
                    .spark
                    .as_ref()
                    .is_some_and(|stats| stats.tok_s > 0.0))
    });
    let changing = view.previous.is_some() && change_elapsed < 0.300;
    if open_progress >= 1.0 && closing_progress == 0.0 && !animating_content && !changing {
        KillTimer(hwnd, ANIMATION_TIMER);
    }
}

unsafe fn hit_at(hwnd: HWND, x: i32, y: i32) -> HeaderHit {
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    VIEW.with(|view| {
        view.borrow().as_ref().map_or(HeaderHit::None, |view| {
            header_hit_test(
                x,
                y,
                client.right,
                view.dpi as f32 / 96.0,
                &view.snapshot.sections,
            )
        })
    })
}

unsafe fn dismiss(hwnd: HWND) {
    close();
    OWNER.with(|owner| PostMessageW(owner.get(), WM_DISMISSED, 0, 0));
    SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 | WS_EX_NOACTIVATE) as isize,
    );
}

unsafe extern "system" fn proc_(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => {
            if is_click_mode() {
                MA_ACTIVATE as LRESULT
            } else {
                MA_NOACTIVATE as LRESULT
            }
        }
        WM_ACTIVATE => {
            if (wp & 0xffff) as u32 == WA_INACTIVE && is_click_mode() && !is_pinned() {
                dismiss(hwnd);
            }
            0
        }
        WM_KEYDOWN => {
            if wp == VK_ESCAPE as usize {
                dismiss(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, message, wp, lp)
        }
        WM_TIMER if wp == ANIMATION_TIMER => {
            redraw(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            HOVER.with(|h| h.set(true));
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            let hit = hit_at(hwnd, x, y);
            if HOVER_HIT.with(|old| old.replace(hit)) != hit {
                redraw(hwnd);
            }
            let cursor = if hit == HeaderHit::None {
                IDC_ARROW
            } else {
                IDC_HAND
            };
            SetCursor(LoadCursorW(null_mut(), cursor));
            let mut e = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut e);
            0
        }
        WM_MOUSE_LEAVE_MSG => {
            HOVER.with(|h| h.set(false));
            if HOVER_HIT.with(|hit| hit.replace(HeaderHit::None)) != HeaderHit::None {
                redraw(hwnd);
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            match hit_at(hwnd, x, y) {
                HeaderHit::Close => dismiss(hwnd),
                HeaderHit::Pin => {
                    PINNED.with(|pinned| pinned.set(!pinned.get()));
                    redraw(hwnd);
                }
                HeaderHit::Refresh => {
                    OWNER.with(|o| PostMessageW(o.get(), WM_REFRESH, 0, 0));
                }
                HeaderHit::Provider(provider) => {
                    OWNER.with(|owner| {
                        PostMessageW(owner.get(), WM_OPEN_PROVIDER, provider as usize, 0)
                    });
                }
                HeaderHit::None if y < 38 && is_pinned() => {
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
                }
                HeaderHit::None => {}
            }
            0
        }
        WM_DESTROY => {
            WINDOW.with(|w| w.set(null_mut()));
            0
        }
        _ => DefWindowProcW(hwnd, message, 0, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_hit_testing_maps_controls_and_providers() {
        let width = MIN_WIDTH as i32;
        let sections = vec![
            Section {
                provider: ProviderKind::Claude,
                ..Default::default()
            },
            Section {
                provider: ProviderKind::Codex,
                ..Default::default()
            },
        ];
        assert_eq!(
            header_hit_test(width - 24, 18, width, 1.0, &sections),
            HeaderHit::Close
        );
        assert_eq!(
            header_hit_test(width - 48, 18, width, 1.0, &sections),
            HeaderHit::Pin
        );
        assert_eq!(
            header_hit_test(width - 72, 18, width, 1.0, &sections),
            HeaderHit::Refresh
        );
        let snapshot = Snapshot {
            sections: sections.clone(),
            ..Default::default()
        };
        let layout = unsafe { card_layout(&snapshot, width, 1.0) };
        for (index, provider) in [ProviderKind::Claude, ProviderKind::Codex]
            .into_iter()
            .enumerate()
        {
            let panel = layout.panels[index].rect;
            assert_eq!(
                header_hit_test(panel.left + 12, panel.top + 12, width, 1.0, &sections),
                HeaderHit::Provider(provider)
            );
            assert_eq!(
                header_hit_test(panel.left + 12, panel.top + 40, width, 1.0, &sections),
                HeaderHit::None
            );
        }
    }

    #[test]
    fn worst_limit_and_status_copy_use_the_most_severe_limit() {
        let bars = vec![
            Bar {
                percent: 20.0,
                elapsed: 40.0,
                projected: 50.0,
                ..Default::default()
            },
            Bar {
                percent: 70.0,
                elapsed: 40.0,
                projected: 100.0,
                full_at: Some("8:52 PM".into()),
                ..Default::default()
            },
        ];
        assert_eq!(worst_limit_index(&bars), Some(1));
        assert_eq!(
            provider_status_text("Claude", &bars),
            "Claude · Runs out 8:52 PM"
        );
        let section = Section {
            provider: ProviderKind::Claude,
            title: "Claude".into(),
            bars,
            ..Default::default()
        };
        assert_eq!(
            status_pill(&section),
            ("Claude · Runs out 8:52 PM".into(), PillState::Crit)
        );
        let stale = Section {
            provider: ProviderKind::Codex,
            title: "Codex".into(),
            stale: true,
            as_of: "6 h old".into(),
            ..Default::default()
        };
        assert_eq!(
            status_pill(&stale),
            ("Codex · Stale · 6 h".into(), PillState::Idle)
        );
    }

    #[test]
    fn age_and_reset_copy_follow_compact_rules() {
        assert_eq!(age_copy(24, false), "24 s ago");
        assert_eq!(age_copy(300, false), "5 m ago");
        assert_eq!(age_copy(21600, true), "6 h old");
        assert_eq!(reset_time_copy(true, "Thu", "10:19 PM"), "10:19 PM");
        assert_eq!(reset_time_copy(false, "Thu", "8:59 AM"), "Thu 8:59 AM");
    }
}
