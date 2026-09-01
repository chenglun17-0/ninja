//! `window-save-state`：保存/恢复用户看见的窗。
//!
//! 一扇窗 = Ninja 自己记下的一个标签条（frame + 有序 tabs）。
//! AppKit 把每个 tab 做成 NSWindow，不能在退出时向它反推组成员。
//! 开窗 / ⌘T / 关 tab 时维护标签条；存盘只读这份名单。

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSWindow, NSWindowOrderingMode};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
use serde::{Deserialize, Serialize};

use crate::host;
use crate::pane::{self, LayoutNode};
use crate::shell;

std::thread_local! {
    /// 主线程：每个元素是用户的一扇窗，Vec 顺序 = 标签栏。
    static STRIPS: RefCell<Vec<Vec<Retained<NSWindow>>>> = const { RefCell::new(Vec::new()) };
    /// ⌘Q 已拍过快照。之后的 windowWillClose 是拆台，不再写 session。
    static QUITTING: Cell<bool> = const { Cell::new(false) };
}

const SESSION_KEY: &str = "ninja.session.v1";

#[derive(Serialize, Deserialize, Default)]
struct Session {
    v: u32,
    windows: Vec<SessionWindow>,
}

#[derive(Serialize, Deserialize)]
struct SessionWindow {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    tabs: Vec<SessionTab>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionTab {
    #[serde(default)]
    pub title_override: Option<String>,
    pub tree: LayoutNode,
}

fn save_state_mode() -> String {
    host::config()
        .and_then(|c| crate::config::get_enum_str(c, "window-save-state"))
        .unwrap_or_else(|| "default".into())
}

fn should_restore() -> bool {
    // 这份 JSON 是 Ninja 自己写的，不是 NSRestorable。
    // 写了就恢复；只有 window-save-state=never 才不写也不读。
    // 不要拿系统「退出时关闭窗口」来丢掉自己的会话。
    save_state_mode() != "never"
}

fn is_terminal_window(w: &NSWindow) -> bool {
    w.tabbingIdentifier().to_string() == shell::TABBING_ID && pane::container_of(w).is_some()
}

fn ptr_of(w: &NSWindow) -> *const NSWindow {
    std::ptr::from_ref(w)
}

fn retain_window(w: &NSWindow) -> Retained<NSWindow> {
    unsafe { Retained::retain(std::ptr::from_ref(w) as *mut NSWindow).expect("window") }
}

fn chrome_window(group: &[Retained<NSWindow>]) -> &NSWindow {
    group
        .iter()
        .find(|w| w.isKeyWindow())
        .or_else(|| group.iter().find(|w| w.isVisible()))
        .map(|w| &**w)
        .unwrap_or(&group[0])
}

fn find_strip(strips: &mut [Vec<Retained<NSWindow>>], w: &NSWindow) -> Option<usize> {
    let p = ptr_of(w);
    strips.iter().position(|st| st.iter().any(|x| ptr_of(x) == p))
}

pub fn begin_quit() {
    QUITTING.with(|q| q.set(true));
}

pub fn is_quitting() -> bool {
    QUITTING.with(|q| q.get())
}

/// ⌘N / 首窗 / 恢复的第一扇窗。
pub fn note_new_window(w: &NSWindow) {
    if !is_terminal_window(w) {
        return;
    }
    let r = retain_window(w);
    STRIPS.with(|s| s.borrow_mut().push(vec![r]));
}

/// ⌘T：`tab` 是 `host` 那一扇窗里的新标签。
pub fn note_new_tab(host: &NSWindow, tab: &NSWindow) {
    if !is_terminal_window(tab) {
        return;
    }
    let tab_r = retain_window(tab);
    STRIPS.with(|s| {
        let mut strips = s.borrow_mut();
        if let Some(i) = find_strip(&mut strips, host) {
            let p = ptr_of(host);
            let pos = strips[i]
                .iter()
                .position(|x| ptr_of(x) == p)
                .unwrap_or(strips[i].len().saturating_sub(1));
            strips[i].insert(pos + 1, tab_r);
        } else {
            strips.push(vec![retain_window(host), tab_r]);
        }
    });
}

/// 关 tab / 关窗。
pub fn note_close(w: &NSWindow) {
    let p = ptr_of(w);
    STRIPS.with(|s| {
        let mut strips = s.borrow_mut();
        for st in strips.iter_mut() {
            st.retain(|x| ptr_of(x) != p);
        }
        strips.retain(|st| !st.is_empty());
    });
}

/// MOVE_TAB 之后：按同一套下标在我们的标签条上挪。
pub fn note_move(selected: &NSWindow, amount: isize) {
    if amount == 0 {
        return;
    }
    let p = ptr_of(selected);
    STRIPS.with(|s| {
        let mut strips = s.borrow_mut();
        let Some(si) = strips.iter().position(|st| st.iter().any(|x| ptr_of(x) == p)) else {
            return;
        };
        let count = strips[si].len();
        if count == 0 {
            return;
        }
        let Some(selected_index) = strips[si].iter().position(|x| ptr_of(x) == p) else {
            return;
        };
        let final_index = if amount < 0 {
            selected_index.saturating_sub((-amount) as usize)
        } else {
            (selected_index + amount as usize).min(count - 1)
        };
        if final_index == selected_index {
            return;
        }
        let tab = strips[si].remove(selected_index);
        strips[si].insert(final_index, tab);
    });
}

/// 存盘/插件槽位共用：Ninja 记下的标签条。
pub fn tab_groups() -> Vec<Vec<Retained<NSWindow>>> {
    STRIPS.with(|s| {
        s.borrow()
            .iter()
            .map(|st| {
                st.iter()
                    .filter(|w| is_terminal_window(w))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|st: &Vec<_>| !st.is_empty())
            .collect()
    })
}

pub fn save() {
    if save_state_mode() == "never" {
        return;
    }
    let mut windows = Vec::new();
    for group in tab_groups() {
        if group.is_empty() {
            continue;
        }
        let f = chrome_window(&group).frame();
        let tabs: Vec<SessionTab> = group
            .iter()
            .filter_map(|tw| {
                let c = pane::container_of(tw)?;
                Some(SessionTab {
                    title_override: c.title_override(),
                    tree: c.dump_layout(),
                })
            })
            .collect();
        if tabs.is_empty() {
            continue;
        }
        windows.push(SessionWindow {
            x: f.origin.x,
            y: f.origin.y,
            w: f.size.width,
            h: f.size.height,
            tabs,
        });
    }
    let json = serde_json::to_string(&Session { v: 1, windows }).unwrap_or_default();
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(SESSION_KEY);
    let val = NSString::from_str(&json);
    unsafe {
        defaults.setObject_forKey(Some(&val), &key);
    }
}

pub fn restore(mtm: MainThreadMarker) -> bool {
    if !should_restore() {
        return false;
    }
    if std::env::var_os("NINJA_P2_SELFTEST").is_some() || std::env::var_os("NINJA_E2E_SCREEN").is_some()
    {
        return false;
    }
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(SESSION_KEY);
    let s: Option<Retained<NSString>> = unsafe { msg_send![&defaults, stringForKey: &*key] };
    let Some(s) = s else {
        return false;
    };
    let Ok(session) = serde_json::from_str::<Session>(&s.to_string()) else {
        return false;
    };
    if session.windows.is_empty() {
        return false;
    }
    let mut any = false;
    for win in session.windows {
        let Some(first_tab) = win.tabs.first() else {
            continue;
        };
        let frame = NSRect::new(NSPoint::new(win.x, win.y), NSSize::new(win.w, win.h));
        let w = shell::make_window_restored(
            mtm,
            ghostty_sys::GHOSTTY_SURFACE_CONTEXT_WINDOW,
            first_tab,
            frame,
        );
        note_new_window(&w);
        for tab in win.tabs.iter().skip(1) {
            let tw = shell::make_window_restored(
                mtm,
                ghostty_sys::GHOSTTY_SURFACE_CONTEXT_TAB,
                tab,
                frame,
            );
            w.addTabbedWindow_ordered(&tw, NSWindowOrderingMode::Above);
            note_new_tab(&w, &tw);
        }
        w.setFrame_display(frame, false);
        w.makeKeyAndOrderFront(None);
        any = true;
    }
    any
}
