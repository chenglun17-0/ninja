//! ninja q0 最小嵌入验证 + 能力审计取证机。
//!
//! 一个 AppKit 窗口挂一个 ghostty surface（宿主自建 NSView 经
//! `surface_config.platform.macos.nsview` 交给 libghostty，Metal 渲染层由
//! ghostty 挂入），跑 /bin/bash，键盘走 `ghostty_surface_key/text`，
//! 渲染由 ghostty Metal 层自动完成。程序按时间线自驱动完成 q0 审计取证
//! （网格读取、hyperlink、屏幕快照、surface 之上合成层、配置运行时改、
//! 键位拦截），把证据写进 --evidence-dir（日志 + 截图 + 报告）。
//!
//! 结论文档：docs/Q0-CAPABILITY-AUDIT.md。

use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSScreen,
    NSPasteboard, NSPasteboardTypeString, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{kCFRunLoopCommonModes, CFRunLoopTimerContext};
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;

use ghostty_sys::*;

// ---------------------------------------------------------------------------
// 全局演示状态（timer 回调在主线程；wakeup 可能来自 IO 线程，只做唤醒）
// ---------------------------------------------------------------------------

struct Demo {
    app: ghostty_app_t,
    surface: ghostty_surface_t,
    config: ghostty_config_t,
    view: Retained<NSView>,
    window: Retained<NSWindow>,
    evidence_dir: PathBuf,
    log: fs::File,
    start: Instant,
    // 审计观察项
    over_link_url: Option<String>,
    cell_size_px: Option<(u32, u32)>,
    size_limit: Option<String>,
    config_change_count: u32,
    key_sequence_events: u32,
    key_table_events: u32,
    pwd: Option<String>,
    title: Option<String>,
    open_url: Option<String>,
    // 时间线进度
    step: u32,
    closing: bool,
    results: Vec<(String, bool, String)>,
    // 取证实际用的屏（PLAN「E2E 虚拟屏幕」：NINJA_E2E_SCREEN 或主屏回退）
    screen_note: String,
}

// SAFETY 不变量：Demo 只在主线程访问（timer_cb / action_cb / 剪贴板回调都
// 发生在主线程的 tick 里；wakeup_cb 只唤醒 RunLoop，不触 DEMO）。
// ghostty_app_tick 会同步重入 action_cb，不能用 Mutex（重入死锁），故用裸指针。
static DEMO_PTR: AtomicPtr<Demo> = AtomicPtr::new(std::ptr::null_mut());

/// 主线程取 Demo（所有调用点都在主线程；wakeup_cb 不用此函数）。
fn d() -> &'static mut Demo {
    d_opt().expect("DEMO not initialized")
}

/// 回调可能在 app_new/surface_new 期间就到达（早于 surface 就位），容忍未初始化。
fn d_opt() -> Option<&'static mut Demo> {
    let p = DEMO_PTR.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}
static WAKEUPS: AtomicU64 = AtomicU64::new(0);
static ALL_PASS: AtomicBool = AtomicBool::new(true);

fn log_line(demo: &mut Demo, msg: &str) {
    let t = demo.start.elapsed().as_secs_f32();
    println!("[{:7.3}s] {msg}", t);
    let _ = writeln!(demo.log, "[{:7.3}s] {msg}", t);
}

fn check(demo: &mut Demo, name: &str, ok: bool, detail: String) {
    if !ok {
        ALL_PASS.store(false, Ordering::SeqCst);
    }
    let verdict = if ok { "PASS" } else { "FAIL" };
    println!("[check] {name}: {verdict} — {detail}");
    let _ = writeln!(demo.log, "[check] {name}: {verdict} — {detail}");
    demo.results.push((name.to_string(), ok, detail));
}

// ---------------------------------------------------------------------------
// ghostty runtime 回调
// ---------------------------------------------------------------------------

unsafe extern "C" fn wakeup_cb(_userdata: *mut c_void) {
    WAKEUPS.fetch_add(1, Ordering::Relaxed);
    // 可能从 IO/渲染线程调用：只唤醒主 RunLoop，主线程 timer 里 app_tick
    //（CFRunLoop 唤醒线程安全；objc2-core-foundation 0.3 的 wake_up 是安全方法）。
    objc2_core_foundation::CFRunLoop::main().unwrap().wake_up();
}

unsafe extern "C" fn action_cb(
    _app: ghostty_app_t,
    _target: ghostty_target_s,
    action: ghostty_action_s,
) -> bool {
    let Some(d) = d_opt() else { return false };
    // SAFETY: action 是联合体，按 tag 取字段。
    unsafe {
        match action.tag {
            GHOSTTY_ACTION_MOUSE_OVER_LINK => {
                let url = std::slice::from_raw_parts(
                    action.action.mouse_over_link.url as *const u8,
                    action.action.mouse_over_link.len,
                );
                let url = String::from_utf8_lossy(url).to_string();
                if !url.is_empty() {
                    log_line(d, &format!("action MOUSE_OVER_LINK url={url}"));
                    d.over_link_url = Some(url);
                }
                true
            }
            GHOSTTY_ACTION_CELL_SIZE => {
                let c = action.action.cell_size;
                log_line(d, &format!("action CELL_SIZE {}x{}px", c.width, c.height));
                d.cell_size_px = Some((c.width, c.height));
                true
            }
            GHOSTTY_ACTION_SIZE_LIMIT => {
                let s = action.action.size_limit;
                let text = format!(
                    "min {}x{} max {}x{}",
                    s.min_width, s.min_height, s.max_width, s.max_height
                );
                log_line(d, &format!("action SIZE_LIMIT {text}"));
                d.size_limit = Some(text);
                true
            }
            GHOSTTY_ACTION_CONFIG_CHANGE => {
                d.config_change_count += 1;
                log_line(d, "action CONFIG_CHANGE");
                true
            }
            GHOSTTY_ACTION_KEY_SEQUENCE => {
                d.key_sequence_events += 1;
                log_line(
                    d,
                    &format!("action KEY_SEQUENCE active={}", action.action.key_sequence.active),
                );
                true
            }
            GHOSTTY_ACTION_KEY_TABLE => {
                d.key_table_events += 1;
                log_line(d, "action KEY_TABLE");
                true
            }
            GHOSTTY_ACTION_PWD => {
                let pwd = CStr::from_ptr(action.action.pwd.pwd)
                    .to_string_lossy()
                    .to_string();
                log_line(d, &format!("action PWD {pwd}"));
                d.pwd = Some(pwd);
                true
            }
            GHOSTTY_ACTION_SET_TITLE => {
                let t = CStr::from_ptr(action.action.set_title.title)
                    .to_string_lossy()
                    .to_string();
                log_line(d, &format!("action SET_TITLE {t}"));
                d.title = Some(t);
                true
            }
            GHOSTTY_ACTION_OPEN_URL => {
                let url = std::slice::from_raw_parts(
                    action.action.open_url.url as *const u8,
                    action.action.open_url.len,
                );
                let url = String::from_utf8_lossy(url).to_string();
                log_line(
                    d,
                    &format!("action OPEN_URL kind={:?} url={url}", action.action.open_url.kind),
                );
                d.open_url = Some(url);
                true
            }
            GHOSTTY_ACTION_MOUSE_SHAPE => {
                // 诊断探针：linkAtPos 命中任何链接都会先发 mouse_shape=pointer
                // （不受 link-previews 门控）。记录一次。
                if action.action.mouse_shape == GHOSTTY_MOUSE_SHAPE_POINTER {
                    log_line(d, "action MOUSE_SHAPE pointer（linkAtPos 命中链接）");
                }
                false
            }
            _ => false, // 其余动作 demo 不需要，交回 ghostty
        }
    }
}

unsafe extern "C" fn read_clipboard_cb(
    _userdata: *mut c_void,
    _clipboard: ghostty_clipboard_e,
    request: *mut c_void,
) -> bool {
    // 主线程（app_tick 处理中）调用。同步回粘贴板内容。
    let Some(dd) = d_opt() else { return false };
    if dd.surface.is_null() {
        return false;
    }
    unsafe {
        let surface = dd.surface;
        let empty = b"\0";
        let pb = NSPasteboard::generalPasteboard();
        let Some(s) = pb.stringForType(NSPasteboardTypeString) else {
            ghostty_surface_complete_clipboard_request(
                surface,
                empty.as_ptr() as *const c_char,
                request,
                false,
            );
            return true;
        };
        let c = CString::new(s.to_string()).unwrap_or_default();
        ghostty_surface_complete_clipboard_request(surface, c.as_ptr(), request, true);
    }
    true
}

unsafe extern "C" fn confirm_read_clipboard_cb(
    _userdata: *mut c_void,
    _data: *const c_char,
    request: *mut c_void,
    _kind: ghostty_clipboard_request_e,
) {
    // demo 无确认 UI：以空内容放行，避免粘贴挂起。
    let Some(dd) = d_opt() else { return };
    if dd.surface.is_null() {
        return;
    }
    unsafe {
        let surface = dd.surface;
        ghostty_surface_complete_clipboard_request(
            surface,
            b"\0".as_ptr() as *const c_char,
            request,
            false,
        );
    }
}

unsafe extern "C" fn write_clipboard_cb(
    _userdata: *mut c_void,
    clipboard: ghostty_clipboard_e,
    contents: *const ghostty_clipboard_content_s,
    count: usize,
    _confirmed: bool,
) {
    unsafe {
        if clipboard != GHOSTTY_CLIPBOARD_STANDARD || count == 0 || contents.is_null() {
            return;
        }
        let data = std::slice::from_raw_parts(
            (*contents).data as *const u8,
            libc::strlen((*contents).data),
        );
        let Ok(s) = std::str::from_utf8(data) else { return };
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        pb.setString_forType(&NSString::from_str(s), NSPasteboardTypeString);
    }
}

unsafe extern "C" fn close_surface_cb(_userdata: *mut c_void, process_alive: bool) {
    let Some(d) = d_opt() else { return };
    log_line(d, &format!("close_surface_cb process_alive={process_alive}"));
    d.closing = true; // 主循环拆卸
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("must be on main thread")
}

// ---------------------------------------------------------------------------
// 时间线（主线程 timer：tick + draw + 逐步取证）
// ---------------------------------------------------------------------------

unsafe extern "C-unwind" fn timer_cb(
    _timer: *mut objc2_core_foundation::CFRunLoopTimer,
    _info: *mut c_void,
) {
    let d = d();
    unsafe { ghostty_app_tick(d.app) };
    unsafe { ghostty_surface_draw(d.surface) };
    let t = d.start.elapsed().as_secs_f32();

    // --- t≈0.4：启动信息 -----------------------------------------------------
    if d.step == 0 && t > 0.4 {
        d.step = 1;
        let size = unsafe { ghostty_surface_size(d.surface) };
        log_line(
            d,
            &format!(
                "surface size: {}x{}px, {}x{} cells, cell {}x{}px",
                size.width_px, size.height_px, size.columns, size.rows,
                size.cell_width_px, size.cell_height_px
            ),
        );
        let tty = unsafe { ghostty_surface_tty_name(d.surface) };
        let tty = unsafe { CStr::from_ptr(tty.ptr) }.to_string_lossy().to_string();
        let pid = unsafe { ghostty_surface_foreground_pid(d.surface) };
        log_line(d, &format!("pty: tty_name={tty} foreground_pid={pid}"));
    }

    // --- t≈0.8：合成键盘输入（surface_text + ENTER 键事件） -----------------
    if d.step == 1 && t > 0.8 {
        d.step = 2;
        let s = "echo TYPED-VIA-SURFACE-TEXT";
        unsafe { ghostty_surface_text(d.surface, s.as_ptr() as *const c_char, s.len()) };
        let key = ghostty_input_key_s {
            action: GHOSTTY_ACTION_PRESS,
            mods: GHOSTTY_MODS_NONE,
            consumed_mods: GHOSTTY_MODS_NONE,
            keycode: GHOSTTY_KEY_ENTER as u32,
            text: b"\r\0".as_ptr() as *const c_char,
            unshifted_codepoint: '\r' as u32,
            composing: false,
        };
        let consumed = unsafe { ghostty_surface_key(d.surface, key) };
        log_line(
            d,
            &format!("input: surface_text(\"{s}\") + key ENTER (consumed={consumed})"),
        );
    }

    // --- t≈1.8：网格读取（全视口） ------------------------------------------
    if d.step == 2 && t > 1.8 {
        d.step = 3;
        let size = unsafe { ghostty_surface_size(d.surface) };
        let read_all =
            unsafe { read_text(d.surface, 0, 0, (size.columns - 1) as u32, (size.rows - 1) as u32) };
        let has_grid =
            read_all.contains("GRID-READ-LINE-1") && read_all.contains("GRID-READ-LINE-2");
        let has_typed = read_all.contains("TYPED-VIA-SURFACE-TEXT");
        log_line(
            d,
            &format!(
                "read_text(viewport) {} bytes, 前 4 行: {:?}",
                read_all.len(),
                read_all.lines().take(4).collect::<Vec<_>>().join(" ⏎ ")
            ),
        );
        fs::write(d.evidence_dir.join("grid-viewport.txt"), &read_all).unwrap();
        check(
            d,
            "grid-read-viewport",
            has_grid && has_typed,
            "read_text(POINT_VIEWPORT) 同时拿到 initial_input 输出与键盘输入回显".into(),
        );
    }

    // --- t≈2.3：网格读取（精确区域——按全视口定位到的行号） ------------------
    if d.step == 3 && t > 2.3 {
        d.step = 4;
        let size = unsafe { ghostty_surface_size(d.surface) };
        let full =
            unsafe { read_text(d.surface, 0, 0, (size.columns - 1) as u32, (size.rows - 1) as u32) };
        let lines: Vec<&str> = full.lines().collect();
        let row1 = lines.iter().position(|l| l.trim_end() == "GRID-READ-LINE-1");
        let Some(r1) = row1 else {
            check(d, "grid-read-region", false, "视口里找不到 GRID-READ-LINE-1".into());
            return;
        };
        let region = unsafe { read_text(d.surface, 0, r1 as u32, 30, r1 as u32 + 1) };
        let ok = region.contains("GRID-READ-LINE-1") && region.contains("GRID-READ-LINE-2");
        fs::write(d.evidence_dir.join("grid-region.txt"), &region).unwrap();
        log_line(d, &format!("read_text(region rows {r1}..{}) = {region:?}", r1 + 1));
        check(
            d,
            "grid-read-region",
            ok,
            format!("read_text 精确网格区域（VIEWPORT 坐标 rows {r1}..{}）", r1 + 1),
        );
    }

    // --- t≈2.8：hyperlink hover（mouse_pos → MOUSE_OVER_LINK） --------------
    if d.step == 4 && t > 2.8 {
        d.step = 5;
        let size = unsafe { ghostty_surface_size(d.surface) };
        let full =
            unsafe { read_text(d.surface, 0, 0, (size.columns - 1) as u32, (size.rows - 1) as u32) };
        let lines: Vec<&str> = full.lines().collect();
        let Some(lr) = lines.iter().position(|l| l.trim_end() == "CLICKABLE-LINK") else {
            check(d, "hyperlink-hover", false, "视口里找不到 CLICKABLE-LINK 行".into());
            return;
        };
        log_line(d, &format!("CLICKABLE-LINK 输出行位于 row {lr}（⌘ 扫描从全网格兜底）"));
        // mouse_pos 语义同 macOS AppKit mouseMoved：view points、原点在「上」
        // （mouseMoved 传 frame.height - pos.y；embedded.zig cursorPosToPixels 只缩放）。
        let scale = d.window.backingScaleFactor() as f64;
        let (cw_pt, ch_pt) = (
            size.cell_width_px as f64 / scale,
            size.cell_height_px as f64 / scale,
        );
        // OSC-8 的 hover 语义（Surface.zig linkAtPos）：只有 ctrlOrSuper 修饰键
        // 按下时才走 OSC-8 分支（macOS Ghostty 同款 ⌘+hover 显示链接）；
        // 无修饰键则只查配置的 link 正则（links.len==0 时无链接）。
        // 全网格扫描（容错：行号定位/内边距偏差不影响结论）。
        let mut got: Option<String> = None;
        let rows = lines.len().min(size.rows as usize);
        for row in 0..rows {
            for col in [3.5, 20.0, 40.0, 55.0] {
                let (x, y) = (cw_pt * col, ch_pt * (row as f64 + 0.5));
                unsafe { ghostty_surface_mouse_pos(d.surface, x, y, GHOSTTY_MODS_SUPER) };
                if let Some(u) = &d.over_link_url {
                    got = Some(u.clone());
                    log_line(
                        d,
                        &format!(
                            "mouse_pos(view pt, mods=⌘) row {row} col {col:.0} -> MOUSE_OVER_LINK 触发"
                        ),
                    );
                    break;
                }
            }
            if got.is_some() {
                break;
            }
        }
        if got.is_none() {
            log_line(
                d,
                &format!("mouse_pos 全网格扫描（{} 行×4 列, mods=⌘）均未触发 MOUSE_OVER_LINK", rows),
            );
        }
    }

    // --- t≈3.4：核对 hover 结果 ---------------------------------------------
    if d.step == 5 && t > 3.4 {
        d.step = 6;
        let url = d.over_link_url.clone();
        let ok = matches!(&url, Some(u) if u.contains("ghostty.org/q0"));
        // link-previews 配置回读（审计取证）
        unsafe {
            let mut v: bool = false;
            ghostty_config_get(
                d.config,
                &mut v as *mut _ as *mut c_void,
                c"link-previews".as_ptr(),
                13,
            );
            log_line(d, &format!("config link-previews = {v}"));
        }
        check(d, "hyperlink-hover", ok, format!("MOUSE_OVER_LINK url={url:?}"));
        // 鼠标移到左上角远离链接（清 hover）
        unsafe { ghostty_surface_mouse_pos(d.surface, 2.0, 2.0, GHOSTTY_MODS_NONE) };
    }

    // --- t≈3.6：截图 1（终端真渲染） ----------------------------------------
    if d.step == 6 && t > 3.6 {
        d.step = 7;
        take_shot(d, "shot1-terminal.png");
    }

    // --- t≈4.1：surface 之上加宿主合成层（subview + CALayer） ----------------
    if d.step == 7 && t > 4.1 {
        d.step = 8;
        unsafe {
            let overlay = NSView::new(mtm());
            overlay.setWantsLayer(true);
            {
                let l = CALayer::new();
                // 50% 透明红
                let comps: [f64; 4] = [1.0, 0.0, 0.0, 0.5];
                if let Some(space) = objc2_core_graphics::CGColorSpace::new_device_rgb() {
                    if let Some(c) = CGColor::new(Some(space.as_ref()), comps.as_ptr()) {
                        l.setBackgroundColor(Some(c.as_ref()));
                    }
                }
                l.setBorderWidth(3.0);
                let b2 = overlay.bounds();
                l.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), b2.size));
                overlay.setLayer(Some(&l));
            }
            let b = d.view.bounds();
            overlay.setFrame(NSRect::new(
                NSPoint::new(b.size.width * 0.58, 0.0),
                NSSize::new(b.size.width * 0.42, b.size.height),
            ));
            d.view.addSubview(&overlay); // 父 view 持有 subview
        }
        log_line(d, "overlay added: 宿主 subview(wantsLayer+CALayer 50%红) 叠在 ghostty Metal 层上方");
    }

    // --- t≈4.7：截图 2（合成层在上） ----------------------------------------
    if d.step == 8 && t > 4.7 {
        d.step = 9;
        take_shot(d, "shot2-overlay.png");
    }

    // --- t≈5.2：配置运行时改（background 16161e → 3a2a5b） ------------------
    if d.step == 9 && t > 5.2 {
        d.step = 10;
        let cfg2 = unsafe {
            load_config(
                &d.evidence_dir,
                "config-change.txt",
                "background = 3a2a5b\nlink-previews = true\nfont-size = 14\n",
            )
        };
        let mut bg = ghostty_config_color_s { r: 0, g: 0, b: 0 };
        unsafe {
            ghostty_config_get(
                cfg2,
                &mut bg as *mut _ as *mut c_void,
                c"background".as_ptr(),
                10,
            );
        }
        unsafe { ghostty_surface_update_config(d.surface, cfg2) };
        log_line(
            d,
            &format!(
                "surface_update_config(background=3a2a5b)，config_get 校验 bg=({},{},{})",
                bg.r, bg.g, bg.b
            ),
        );
        unsafe { ghostty_config_free(cfg2) };
    }

    // --- t≈6.4：截图 3（配置生效） -------------------------------------------
    if d.step == 10 && t > 6.4 {
        d.step = 11;
        take_shot(d, "shot3-config-change.png");
        let ok = d.config_change_count > 0;
        check(
            d,
            "config-runtime-change",
            ok,
            format!(
                "surface_update_config 生效（见 shot3 背景变化），CONFIG_CHANGE 回调 {} 次",
                d.config_change_count
            ),
        );
    }

    // --- t≈6.6：键位拦截 ------------------------------------------------------
    if d.step == 11 && t > 6.6 {
        d.step = 12;
        unsafe {
            let cmd_t = key_event(GHOSTTY_KEY_T as u32, GHOSTTY_MODS_SUPER, b"t\0");
            let plain_a = key_event(GHOSTTY_KEY_A as u32, GHOSTTY_MODS_NONE, b"a\0");
            let is_bind_cfg_t = ghostty_config_key_is_binding(d.config, cmd_t);
            let is_bind_cfg_a = ghostty_config_key_is_binding(d.config, plain_a);
            let mut flags: ghostty_binding_flags_e = 0;
            let is_bind_surf_t = ghostty_surface_key_is_binding(d.surface, cmd_t, &mut flags);
            log_line(
                d,
                &format!(
                    "key_is_binding: config(⌘T)={is_bind_cfg_t} config(A)={is_bind_cfg_a} surface(⌘T)={is_bind_surf_t} flags={flags}"
                ),
            );
            check(
                d,
                "key-binding-intercept",
                is_bind_cfg_t && !is_bind_cfg_a && is_bind_surf_t,
                "config/surface_key_is_binding：⌘T 判定为绑定、A 不是（可在派发前拦截）".into(),
            );
        }
    }

    // --- t≈7.2：收尾报告 ------------------------------------------------------
    if d.step == 12 && t > 7.2 {
        d.step = 13;
        let all_pass = d.results.iter().all(|(_, ok, _)| *ok);
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut report = format!(
            "# ninja q0 embed demo report\n# unix {unix}\n# libghostty a887df42 (zig 0.15.2, static libghostty-internal.a)\n\n"
        );
        report.push_str(&format!(
            "wakeup_cb invocations: {}\n",
            WAKEUPS.load(Ordering::Relaxed)
        ));
        report.push_str(&format!("screen: {}\n", d.screen_note));
        if let Some((w, h)) = d.cell_size_px {
            report.push_str(&format!("cell size: {w}x{h}px (CELL_SIZE action)\n"));
        }
        if let Some(sl) = &d.size_limit {
            report.push_str(&format!("size limit: {sl} (SIZE_LIMIT action)\n"));
        }
        report.push_str(&format!("config_change actions: {}\n", d.config_change_count));
        report.push_str(&format!("key_sequence actions: {}\n", d.key_sequence_events));
        report.push_str(&format!("key_table actions: {}\n", d.key_table_events));
        if let Some(p) = &d.pwd {
            report.push_str(&format!("pwd action: {p}\n"));
        }
        if let Some(t) = &d.title {
            report.push_str(&format!("title action: {t}\n"));
        }
        report.push_str("\n[checks]\n");
        for (name, ok, detail) in &d.results {
            report.push_str(&format!(
                "{} {name} — {detail}\n",
                if *ok { "PASS" } else { "FAIL" }
            ));
        }
        report.push_str(&format!(
            "\noverall: {}\n",
            if all_pass { "PASS" } else { "FAIL" }
        ));
        fs::write(d.evidence_dir.join("report.txt"), &report).unwrap();
        log_line(d, &format!("report written: {}/report.txt", d.evidence_dir.display()));
        log_line(d, &format!("OVERALL: {}", if all_pass { "PASS" } else { "FAIL" }));

        // NSApp.run 的 stop 返回不可靠（stop+哑事件在 timer 回调内不生效），
        // 这里显式按序拆卸后直接退出（demo 进程，无常驻资源）。
        log_line(d, "teardown: surface_free → app_free → config_free → exit");
        let (gapp, surface, config) = (d.app, d.surface, d.config);
        unsafe {
            ghostty_surface_free(surface);
            ghostty_app_free(gapp);
            ghostty_config_free(config);
        }
        std::process::exit(if all_pass { 0 } else { 1 });
    }

    if d.step < 13 && t > 20.0 {
        log_line(d, "watchdog: 20s 超时，强制退出（时间线未走完）");
        std::process::exit(2);
    }
}

fn rl_add_timer(rl: &objc2_core_foundation::CFRunLoop, timer: &objc2_core_foundation::CFRunLoopTimer) {
    unsafe { rl.add_timer(Some(timer), kCFRunLoopCommonModes) };
}

fn rl_remove_timer(
    rl: &objc2_core_foundation::CFRunLoop,
    timer: &objc2_core_foundation::CFRunLoopTimer,
) {
    unsafe { rl.remove_timer(Some(timer), kCFRunLoopCommonModes) };
}

fn key_event(keycode: u32, mods: ghostty_input_mods_e, text: &[u8]) -> ghostty_input_key_s {
    ghostty_input_key_s {
        action: GHOSTTY_ACTION_PRESS,
        mods,
        consumed_mods: GHOSTTY_MODS_NONE,
        keycode,
        text: text.as_ptr() as *const c_char,
        unshifted_codepoint: text[0] as u32,
        composing: false,
    }
}

/// VIEWPORT 坐标精确区域读取（网格读取审计项的实现证据）。
unsafe fn read_text(surface: ghostty_surface_t, x0: u32, y0: u32, x1: u32, y1: u32) -> String {
    unsafe {
        let sel = ghostty_selection_s {
            top_left: ghostty_point_s {
                tag: GHOSTTY_POINT_VIEWPORT,
                coord: GHOSTTY_POINT_COORD_EXACT,
                x: x0,
                y: y0,
            },
            bottom_right: ghostty_point_s {
                tag: GHOSTTY_POINT_VIEWPORT,
                coord: GHOSTTY_POINT_COORD_EXACT,
                x: x1,
                y: y1,
            },
            rectangle: true,
        };
        let mut text = ghostty_text_s {
            tl_px_x: 0.0,
            tl_px_y: 0.0,
            offset_start: 0,
            offset_len: 0,
            text: std::ptr::null(),
            text_len: 0,
        };
        let ok = ghostty_surface_read_text(surface, sel, &mut text);
        let out = if ok && !text.text.is_null() {
            let bytes = std::slice::from_raw_parts(text.text as *const u8, text.text_len);
            String::from_utf8_lossy(bytes).to_string()
        } else {
            String::new()
        };
        ghostty_surface_free_text(surface, &mut text);
        out
    }
}

fn take_shot(d: &mut Demo, name: &str) {
    let win = d.window.windowNumber();
    let path = d.evidence_dir.join(name);
    let out = Command::new("screencapture")
        .args(["-x", "-l", &win.to_string()])
        .arg(&path)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let meta = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            log_line(d, &format!("screenshot {name} ({meta} bytes)"));
        }
        Ok(o) => log_line(
            d,
            &format!("screenshot {name} FAILED: {}", String::from_utf8_lossy(&o.stderr)),
        ),
        Err(e) => log_line(d, &format!("screenshot {name} FAILED: {e}")),
    }
}

unsafe fn load_config(evidence_dir: &PathBuf, fname: &str, body: &str) -> ghostty_config_t {
    unsafe {
        let path = evidence_dir.join(fname);
        fs::write(&path, body).unwrap();
        let cpath = CString::new(path.to_str().unwrap()).unwrap();
        let cfg = ghostty_config_new();
        ghostty_config_load_default_files(cfg);
        ghostty_config_load_file(cfg, cpath.as_ptr());
        ghostty_config_finalize(cfg);
        cfg
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut evidence_dir = PathBuf::from("docs/q0-evidence");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--evidence-dir" => {
                evidence_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            other => {
                eprintln!("unknown arg {other}; usage: ninja-embed [--evidence-dir DIR]");
                std::process::exit(2);
            }
        }
    }
    fs::create_dir_all(&evidence_dir).unwrap();
    let log = fs::File::create(evidence_dir.join("demo.log")).unwrap();
    println!("ninja q0 embed demo — evidence: {}", evidence_dir.display());

    unsafe {
        // 1. ghostty 全局初始化
        assert_eq!(ghostty_init(0, std::ptr::null_mut()), 0, "ghostty_init failed");
        let info = ghostty_info();
        let version = std::str::from_utf8(std::slice::from_raw_parts(
            info.version as *const u8,
            info.version_len,
        ))
        .unwrap()
        .to_string();
        println!("libghostty {version} mode={:?}", info.build_mode);

        // 2. 配置（link-previews=true 才发 MOUSE_OVER_LINK；初始背景 16161e）
        let config = load_config(
            &evidence_dir,
            "config-initial.txt",
            "background = 16161e\nlink-previews = true\nfont-size = 14\n",
        );

        // 3. AppKit 壳：窗口 + 宿主自建 NSView
        let mtm = mtm();
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let view = NSView::new(mtm);
        let frame = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(940.0, 620.0));
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setTitle(&NSString::from_str("ninja q0 — libghostty embed"));
        window.setContentView(Some(&view));

        // NINJA_E2E_SCREEN=<displayID>（PLAN「E2E 虚拟屏幕」增补）：窗口落到指定
        // 虚拟屏（按 deviceDescription NSScreenNumber 匹配），不打扰主屏；
        // 未设置/未匹配 → 主屏兜底，取证标注实际用的屏。
        let mut screen_note = String::from("default(main)");
        if let Ok(id) = std::env::var("NINJA_E2E_SCREEN") {
            match id.trim().parse::<u32>() {
                Ok(target) => {
                    let key = NSString::from_str("NSScreenNumber");
                    let matched = NSScreen::screens(mtm).iter().find(|s| {
                        let desc = unsafe { s.deviceDescription() };
                        let v: Option<Retained<objc2_foundation::NSObject>> =
                            unsafe { msg_send![&*desc, objectForKey: &*key] };
                        v.map(|v| {
                            let num: isize = unsafe { msg_send![&*v, integerValue] };
                            num as u32 == target
                        })
                        .unwrap_or(false)
                    });
                    if let Some(s) = matched {
                        let vf = s.visibleFrame();
                        let w = (vf.size.width - 24.0).min(940.0).max(320.0);
                        let h = (vf.size.height - 24.0).min(620.0).max(240.0);
                        unsafe { window.setContentSize(NSSize::new(w, h)) };
                        window.setFrameOrigin(NSPoint::new(
                            vf.origin.x + (vf.size.width - w) / 2.0,
                            vf.origin.y + (vf.size.height - h) / 2.0,
                        ));
                        screen_note = format!("NINJA_E2E_SCREEN={target} (虚拟屏)");
                    } else {
                        screen_note = format!("NINJA_E2E_SCREEN={target} 未匹配，回退主屏");
                    }
                }
                Err(_) => screen_note = format!("NINJA_E2E_SCREEN={id:?} 非法，回退主屏"),
            }
        }
        println!("screen: {screen_note}");
        {
            let _ = writeln!(&log, "screen: {screen_note}");
        }

        window.makeKeyAndOrderFront(None);
        app.activateIgnoringOtherApps(true);

        let scale = window
            .screen()
            .map(|s| s.backingScaleFactor() as f64)
            .unwrap_or(2.0);

        // 4. ghostty app + runtime 回调
        let rt = ghostty_runtime_config_s {
            userdata: std::ptr::null_mut(),
            supports_selection_clipboard: false,
            wakeup_cb: Some(wakeup_cb),
            action_cb: Some(action_cb),
            read_clipboard_cb: Some(read_clipboard_cb),
            confirm_read_clipboard_cb: Some(confirm_read_clipboard_cb),
            write_clipboard_cb: Some(write_clipboard_cb),
            close_surface_cb: Some(close_surface_cb),
        };
        // 状态先就位：app_new/surface_new 期间就可能收到 action 回调
        // （CELL_SIZE / SIZE_LIMIT 等），不能丢审计观察项。
        {
            let demo = Box::new(Demo {
                app: std::ptr::null_mut(),
                surface: std::ptr::null_mut(),
                config,
                view: view.clone(),
                window: window.clone(),
                evidence_dir: evidence_dir.clone(),
                log,
                start: Instant::now(),
                over_link_url: None,
                cell_size_px: None,
                size_limit: None,
                config_change_count: 0,
                key_sequence_events: 0,
                key_table_events: 0,
                pwd: None,
                title: None,
                open_url: None,
                step: 0,
                closing: false,
                results: Vec::new(),
                screen_note,
            });
            DEMO_PTR.store(Box::into_raw(demo), Ordering::Release);
        }

        let gapp = ghostty_app_new(&rt, config);
        assert!(!gapp.is_null(), "ghostty_app_new failed");
        ghostty_app_set_focus(gapp, true);

        // 5. surface：nsview 交给 ghostty；跑 /bin/bash；initial_input 铺证据行
        let mut scfg = ghostty_surface_config_new();
        scfg.platform_tag = GHOSTTY_PLATFORM_MACOS;
        scfg.platform.macos.nsview = Retained::as_ptr(&view) as *mut c_void;
        scfg.userdata = std::ptr::null_mut();
        scfg.scale_factor = scale;
        scfg.font_size = 0.0; // 继承 config 的 font-size
        let command = CString::new("/bin/bash").unwrap();
        scfg.command = command.as_ptr();
        let initial = CString::new(concat!(
            "clear\n",
            "printf 'GRID-READ-LINE-1\\nGRID-READ-LINE-2\\n'\n",
            "printf '\\e]8;;https://ghostty.org/q0\\e\\\\CLICKABLE-LINK\\e]8;;\\e\\\\\\n'\n",
        ))
        .unwrap();
        scfg.initial_input = initial.as_ptr();
        scfg.wait_after_command = false;
        scfg.context = GHOSTTY_SURFACE_CONTEXT_WINDOW;
        let surface = ghostty_surface_new(gapp, &scfg);
        assert!(!surface.is_null(), "ghostty_surface_new failed");

        let b = view.bounds();
        ghostty_surface_set_content_scale(surface, scale, scale);
        ghostty_surface_set_size(
            surface,
            (b.size.width * scale) as u32,
            (b.size.height * scale) as u32,
        );
        ghostty_surface_set_focus(surface, true);

        // 6. surface/app 句柄补进状态 + 主 RunLoop timer（tick + draw + 时间线）
        {
            let d = d();
            d.app = gapp;
            d.surface = surface;
            d.start = Instant::now(); // 时间线以 surface 就绪起算
        }

        let rl = objc2_core_foundation::CFRunLoop::main().unwrap();
        let mut ctx = CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        let timer = objc2_core_foundation::CFRunLoopTimer::new(
            None, 0.05, 0.016, 0, 0, Some(timer_cb), &mut ctx,
        )
        .expect("timer create");
        rl_add_timer(&rl, timer.as_ref());

        // 7. 主事件循环：NSApp.run（AppKit 会在其中跑主 RunLoop）。
        //    时间线走完后 timer_cb 里显式拆卸并 exit（NSApp.stop 不可靠）。
        app.run();

        rl_remove_timer(&rl, timer.as_ref());
        let (gapp, surface, config, mut d) = {
            let p = DEMO_PTR.swap(std::ptr::null_mut(), Ordering::AcqRel);
            assert!(!p.is_null(), "demo state already consumed");
            let d = Box::from_raw(p);
            (d.app, d.surface, d.config, d)
        };
        log_line(&mut d, "teardown: surface_free → app_free → config_free");
        ghostty_surface_free(surface);
        ghostty_app_free(gapp);
        ghostty_config_free(config);
    }

    if !ALL_PASS.load(Ordering::SeqCst) {
        std::process::exit(1);
    }
}
