//! q1 分屏容器：一个窗口/tab = 一个 [`PaneContainer`]（NSView），持一棵
//! pane 树（叶子 = `SurfaceHostView` = 嵌入 surface，内部节点 = 带 ratio
//! 的二叉 split）。移植自 v1 crates/ninja/src/pane.rs（p2/X3 资产），
//! 叶子从自研 TerminalView 换成 libghostty surface：
//! - 布局：`relayout` 递归按 ratio 切 bounds，叶子 `setFrame` →
//!   `setFrameSize` → surface_set_size（resize 全链）；
//! - 分隔条：子 NSView（可拖调 ratio）；
//! - zoom（⌘⇧Enter）：放大叶占满、其余隐藏**不销毁**
//!   （surface 数据继续喂不丢、隐藏面 set_occlusion(false) 停画，
//!   网格冻结在分屏尺寸——还原即正确显示；等价 v1 语义）；
//! - 关闭：多 pane 拆叶（surface 延迟 free，见 [`crate::host`]），
//!   单 pane performClose 关 tab/窗。
//!
//! 菜单动作（split/关 pane/焦点导航/zoom）实现在本类上：first responder
//! （SurfaceHostView）不接的动作沿响应链冒泡到 superview = 本容器。

#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{NSEvent, NSFocusRingType, NSResponder, NSView, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use serde::{Deserialize, Serialize};

use crate::surface::{as_responder, SurfaceHostView};

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum LayoutNode {
    Leaf {
        #[serde(default)]
        pwd: Option<String>,
    },
    Split {
        dir: String,
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

/// 分隔条厚度（points；命中区即此厚度）。
pub const DIVIDER: f64 = 5.0;
/// ratio 夹取范围：两侧叶子各保最小占比。
const RATIO_MIN: f64 = 0.15;
const RATIO_MAX: f64 = 0.85;

/// ⌘⇧Enter 决策（纯逻辑，可单测；v1 X3 原样）：
/// - 单 pane（无分屏）→ 窗口 zoom（最大化非全屏）；
/// - 有分屏 + 未放大 → 放大焦点 pane（无可用目标 = 无操作）；
/// - 有分屏 + 已放大 → 还原布局。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ZoomDecision {
    WindowZoom,
    ZoomFocused,
    Restore,
    None,
}

pub fn zoom_decision(leaf_count: usize, zoomed: bool, has_target: bool) -> ZoomDecision {
    if leaf_count <= 1 {
        ZoomDecision::WindowZoom
    } else if zoomed {
        ZoomDecision::Restore
    } else if has_target {
        ZoomDecision::ZoomFocused
    } else {
        ZoomDecision::None
    }
}

/// 分屏方向：Horizontal = 左右排（⌘D 右分），Vertical = 上下排（⌘⇧D 下分）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Horizontal,
    Vertical,
}

/// pane 树。frame 由 `relayout` 缓存（焦点导航/拖拽换算用）。
enum Node {
    Leaf {
        view: Retained<SurfaceHostView>,
        frame: NSRect,
    },
    Split {
        dir: Dir,
        id: u64,
        ratio: f64,
        first: Box<Node>,
        second: Box<Node>,
        frame: NSRect,
    },
}

/// 把 `rect` 按 `ratio` 与方向切成（first, second, divider）三块。
/// 纯函数（布局单测用；v1 原样）。
pub fn split_rects(rect: NSRect, dir: Dir, ratio: f64) -> (NSRect, NSRect, NSRect) {
    let ratio = ratio.clamp(RATIO_MIN, RATIO_MAX);
    let (first_len, second_len) = match dir {
        Dir::Horizontal => {
            let a = (rect.size.width * ratio - DIVIDER / 2.0).max(0.0);
            (a, (rect.size.width - a - DIVIDER).max(0.0))
        }
        Dir::Vertical => {
            let a = (rect.size.height * ratio - DIVIDER / 2.0).max(0.0);
            (a, (rect.size.height - a - DIVIDER).max(0.0))
        }
    };
    let first = NSRect {
        origin: rect.origin,
        size: match dir {
            Dir::Horizontal => NSSize {
                width: first_len,
                height: rect.size.height,
            },
            Dir::Vertical => NSSize {
                width: rect.size.width,
                height: first_len,
            },
        },
    };
    let second_origin = match dir {
        Dir::Horizontal => NSPoint {
            x: rect.origin.x + first_len + DIVIDER,
            y: rect.origin.y,
        },
        Dir::Vertical => NSPoint {
            x: rect.origin.x,
            y: rect.origin.y + first_len + DIVIDER,
        },
    };
    let second = NSRect {
        origin: second_origin,
        size: match dir {
            Dir::Horizontal => NSSize {
                width: second_len,
                height: rect.size.height,
            },
            Dir::Vertical => NSSize {
                width: rect.size.width,
                height: second_len,
            },
        },
    };
    let divider = match dir {
        Dir::Horizontal => NSRect {
            origin: NSPoint {
                x: rect.origin.x + first_len,
                y: rect.origin.y,
            },
            size: NSSize {
                width: DIVIDER,
                height: rect.size.height,
            },
        },
        Dir::Vertical => NSRect {
            origin: NSPoint {
                x: rect.origin.x,
                y: rect.origin.y + first_len,
            },
            size: NSSize {
                width: rect.size.width,
                height: DIVIDER,
            },
        },
    };
    (first, second, divider)
}

pub struct Ivars {
    tree: RefCell<Option<Node>>,
    next_id: Cell<u64>,
    dividers: RefCell<HashMap<u64, Retained<DividerView>>>,
    /// 当前放大的叶子（None = 未放大）。放大态下其余叶子与分隔条
    /// setHidden 隐藏但**不销毁**：surface 数据继续喂不丢（隐藏面不出帧，
    /// occlusion=false 停画），还原即重显正确内容。
    zoomed: RefCell<Option<Retained<SurfaceHostView>>>,
    title_override: RefCell<Option<String>>,
    last_osc_title: RefCell<Option<String>>,
}

define_class!(
    // SAFETY:
    // - NSView 子类化无强约束方法；覆写的（isFlipped/setFrameSize/drawRect/
    //   动作方法）先走 super 或纯自算。
    // - 不实现 Drop；ivars 在 set_ivars 后只经 RefCell/Cell 访问。
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct PaneContainer;

    impl PaneContainer {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true // 左上原点，与 SurfaceHostView 一致
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(focusRingType))]
        fn focus_ring_type(&self) -> NSFocusRingType {
            NSFocusRingType::None
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {}

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.relayout();
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            let _: () = unsafe { msg_send![super(self), viewDidMoveToWindow] };
            self.relayout();
        }

        /// 背景填生效色板底色（ghostty config background，q1 起 host 读）：
        /// 分隔条缝隙/角落不露白（v1 T2 经验）。
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            crate::host::bg_color().set();
            objc2_app_kit::NSRectFill(self.bounds());
        }

        // q2：菜单动作全部改由 AppDelegate 经 ghostty_surface_binding_action
        // 驱动（键位同源，见 app.rs/perform_menu_binding）；q1 的容器侧菜单
        // 方法（ninjaSplitRight: 等 10 个 selector）已删（平行键位层不复活）。
    }
);

// ---------------------------------------------------------------------------
// Rust 接口
// ---------------------------------------------------------------------------

/// NSView 是否 PaneContainer（isKindOfClass 包装）。
pub fn is_container(v: &NSView) -> bool {
    // SAFETY: isKindOfClass: 任意 NSObject 可查。
    unsafe { objc2::msg_send![v, isKindOfClass: PaneContainer::class()] }
}

/// NSView → &PaneContainer（is_container 已验证后调用）。
///
/// 返回引用按 &'static 交付（同 v1 惯例的裸指针上转）：容器对象与其
/// 窗口 contentView 同寿命，窗口 releasedWhenClosed=NO 由 AppDelegate
/// 注册表持有到 prune 拍——主线程立即使用、不跨 prune 拍存放即安全。
pub fn downcast_container(v: &NSView) -> &'static PaneContainer {
    unsafe { &*(std::ptr::from_ref(v) as *const PaneContainer) }
}

/// 窗口的 contentView 是否 PaneContainer；是则返回它（寿命纪律见
/// [`downcast_container`]）。
pub fn container_of(w: &objc2_app_kit::NSWindow) -> Option<&'static PaneContainer> {
    let content = w.contentView()?;
    if !is_container(&content) {
        return None;
    }
    Some(downcast_container(&content))
}

impl PaneContainer {
    /// 建容器 + 首个叶子（surface 由 [`crate::host::attach_surface`] 挂，
    /// 建 surface 需要 nsview 已就位）。
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let first = SurfaceHostView::new(mtm);
        let frame = first.frame();
        let this = PaneContainer::alloc(mtm).set_ivars(Ivars {
            tree: RefCell::new(None),
            next_id: Cell::new(1),
            dividers: RefCell::new(HashMap::new()),
            zoomed: RefCell::new(None),
            title_override: RefCell::new(None),
            last_osc_title: RefCell::new(None),
        });
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        let view: Retained<PaneContainer> =
            unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setFocusRingType(NSFocusRingType::None);
        view.setClipsToBounds(true);
        view.addSubview(&first);
        *view.ivars().tree.borrow_mut() = Some(Node::Leaf {
            view: first,
            frame,
        });
        view.relayout();
        view
    }

    /// 首个叶子（建窗时挂 surface 用）。
    pub fn first_leaf(&self) -> Retained<SurfaceHostView> {
        self.leaves()[0].clone()
    }

    pub fn title_override(&self) -> Option<String> {
        self.ivars().title_override.borrow().clone()
    }

    pub fn set_title_override(&self, title: Option<String>) {
        *self.ivars().title_override.borrow_mut() = title;
        self.apply_title();
    }

    pub fn set_last_osc_title(&self, title: String) {
        *self.ivars().last_osc_title.borrow_mut() = Some(title);
        if self.title_override().is_none() {
            self.apply_title();
        }
    }

    fn apply_title(&self) {
        let Some(w) = self.window() else {
            return;
        };
        let title = self
            .title_override()
            .or_else(|| self.ivars().last_osc_title.borrow().clone())
            .unwrap_or_else(|| "ninja".into());
        if w.title().to_string() != title {
            w.setTitle(&objc2_foundation::NSString::from_str(&title));
            crate::shell::suppress_titlebar_sampling(&w);
        }
    }

    pub fn dump_layout(&self) -> LayoutNode {
        let tree = self.ivars().tree.borrow();
        dump_node(tree.as_ref())
    }

    /// 用会话快照换掉占位叶子并挂 surface。
    pub fn restore_layout(&self, dump: &LayoutNode, context: ghostty_sys::ghostty_surface_context_e) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        for leaf in self.leaves() {
            if leaf.surface_opt().is_some() {
                crate::host::close_leaf_now(&leaf);
            }
            leaf.removeFromSuperview();
        }
        {
            let mut ds = self.ivars().dividers.borrow_mut();
            for d in ds.values() {
                d.removeFromSuperview();
            }
            ds.clear();
        }
        let node = materialize(self, dump, mtm);
        self.set_tree_and_layout(node);
        let leaves = self.leaves();
        let Some(first) = leaves.first() else {
            return;
        };
        let pwds = collect_pwds(dump);
        crate::host::attach_surface(first, context, None, pwds.first().cloned().flatten());
        for (i, leaf) in leaves.iter().enumerate().skip(1) {
            crate::host::attach_surface(
                leaf,
                ghostty_sys::GHOSTTY_SURFACE_CONTEXT_SPLIT,
                Some(first),
                pwds.get(i).cloned().flatten(),
            );
        }
    }

    /// 当前焦点叶子（first responder 是本容器的某个 SurfaceHostView）。
    pub fn focused_leaf(&self) -> Option<Retained<SurfaceHostView>> {
        let window = self.window()?;
        let responder = window.firstResponder()?;
        let leaves = self.leaves();
        leaves.into_iter().find(|v| {
            std::ptr::eq(
                responder.as_ref() as *const NSResponder,
                as_responder(v) as *const NSResponder,
            )
        })
    }

    /// DFS 叶子（first 在前）。
    pub fn leaves(&self) -> Vec<Retained<SurfaceHostView>> {
        let tree = self.ivars().tree.borrow();
        let mut out = Vec::new();
        collect_leaves(tree.as_ref(), &mut out);
        out
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves().len()
    }

    /// ⌘D/⌘⇧D（ghostty NEW_SPLIT）：在目标叶子旁插一个新 pane
    /// （`before`=左分/上分，默认右分/下分）。新 pane 夺焦。
    /// 放大态下先还原（新 split 的几何才有意义；v1 X3 同款）。
    pub fn split_focused(&self, dir: Dir, before: bool) {
        let Some(target) = self
            .focused_leaf()
            .or_else(|| self.leaves().first().cloned())
        else {
            return;
        };
        self.split_beside(&target, dir, before);
    }

    /// 指定叶子旁插新 pane（ghostty NEW_SPLIT 的 target 即焦点面，
    /// 宿主菜单/钩子可能指定其它叶子）。
    pub fn split_beside(&self, target: &SurfaceHostView, dir: Dir, before: bool) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        self.unzoom();
        if !self.contains(target) {
            return;
        }

        let new_view = SurfaceHostView::new(mtm);
        // surface 先挂（需要 nsview；父容器已就位即可，尺寸 relayout 补）。
        crate::host::attach_surface(
            &new_view,
            ghostty_sys::GHOSTTY_SURFACE_CONTEXT_SPLIT,
            Some(target),
            None,
        );
        self.addSubview(&new_view);

        let id = self.ivars().next_id.get();
        self.ivars().next_id.set(id + 1);
        let divider = DividerView::new(mtm, id);
        self.addSubview(&divider);
        self.ivars().dividers.borrow_mut().insert(id, divider);

        let mut tree = self.take_tree();
        if insert_beside(&mut tree, target, new_view.clone(), dir, id, before) {
            self.set_tree_and_layout(tree);
        } else {
            // 不应发生（target 已验在树里）；防御性回收。
            self.set_tree_and_layout(tree);
            crate::host::close_leaf_now(&new_view);
            if let Some(d) = self.ivars().dividers.borrow_mut().remove(&id) {
                d.removeFromSuperview();
            }
            return;
        }

        // 新 pane 夺焦（iTerm/Ghostty 习惯）。
        if let Some(w) = self.window() {
            w.makeFirstResponder(Some(as_responder(&new_view)));
        }
    }

    /// 关一个 pane：树里只剩它 → 关窗（windowWillClose 拆全部）；
    /// 否则从树里拆掉（surface 经 host 延迟 free——close_surface_cb 可能
    /// 在 ghostty 调用栈深处触发，立即 free 会重入拆栈）。
    /// v1 SIGSEGV 教训全保留：先转移焦点再拆视图。
    pub fn close_leaf(&self, view: &SurfaceHostView) {
        if self.leaf_count() <= 1 {
            // 最后一个 pane：整窗/tab 关（windowShouldClose 的裸⌘W 决策
            // 在此路径放行——单 pane 放行原生语义）。
            if let Some(w) = self.window() {
                w.performClose(None);
            }
            return;
        }
        // 关的正是放大面 → 撤销放大态，其余 pane 露回。
        let closing_zoomed = self
            .ivars()
            .zoomed
            .borrow()
            .as_ref()
            .is_some_and(|z| same_view(z, view));
        if closing_zoomed {
            self.ivars().zoomed.borrow_mut().take();
            for v in self.leaves() {
                if !same_view(&v, view) {
                    v.setHidden(false);
                    v.set_surface_occlusion(true);
                }
            }
            for d in self.ivars().dividers.borrow().values() {
                d.setHidden(false);
            }
        }
        // 先把焦点从待拆 pane 挪走（NSWindow 的 firstResponder 不额外
        // 持引用，先 resign 再释放视图，否则窗口后续事件路径触已释放
        // 对象——p2 实测关 pane/关窗 SEGFAULT 根因，v1 原样保留）。
        if let Some(w) = self.window() {
            let removing_is_focused =
                self.focused_leaf().is_some_and(|f| same_view(&f, view));
            if removing_is_focused {
                let other = self
                    .leaves()
                    .into_iter()
                    .find(|v| !same_view(v, view));
                if let Some(o) = other {
                    w.makeFirstResponder(Some(as_responder(&o)));
                }
            }
        }
        // surface 延迟 free + 视图立即出树（host 持 Retained 到 free 完）。
        crate::host::close_leaf_deferred(view);
        view.removeFromSuperview();
        let tree = self.take_tree();
        let mut dropped = Vec::new();
        let new_tree = remove_leaf(tree, view, &mut dropped);
        for id in dropped {
            if let Some(d) = self.ivars().dividers.borrow_mut().remove(&id) {
                d.removeFromSuperview();
            }
        }
        if let Some(t) = new_tree {
            self.set_tree_and_layout(t);
        }
        // 焦点可能随 pane 消失：交给第一个叶子。
        let leaves = self.leaves();
        if self.focused_leaf().is_none()
            && let (Some(first), Some(w)) = (leaves.first(), self.window())
        {
            w.makeFirstResponder(Some(as_responder(first)));
        }
    }

    /// 关窗/退出前收尾全部叶子（幂等；EOF 先到过的叶子再走一遍无害）。
    /// 只拆 surface（延迟 free），**不**碰视图层级：windowWillClose 期间
    /// AppKit 的收尾还会触碰子视图，过早 removeFromSuperview 会留悬空
    /// 指针（p2 实测关窗 SEGFAULT，v1 原样保留）。
    pub fn shutdown_all(&self) {
        for v in self.leaves() {
            crate::host::close_leaf_deferred(&v);
        }
    }

    /// 焦点方向导航：按叶子 frame 找相邻重叠面上最近的那个（v1 原样）。
    pub fn focus_dir(&self, dir: Dir, forward: bool) {
        self.unzoom();
        let Some(from) = self.focused_leaf() else { return };
        let Some((_, from_frame)) = self
            .leaves_with_frames()
            .into_iter()
            .find(|(v, _)| same_view(v, &from))
        else {
            return;
        };
        let mut best: Option<(f64, Retained<SurfaceHostView>)> = None;
        for (v, f) in self.leaves_with_frames() {
            if same_view(&v, &from) {
                continue;
            }
            let overlap = match dir {
                Dir::Horizontal => {
                    (f.origin.y + f.size.height).min(from_frame.origin.y + from_frame.size.height)
                        - f.origin.y.max(from_frame.origin.y)
                }
                Dir::Vertical => {
                    (f.origin.x + f.size.width).min(from_frame.origin.x + from_frame.size.width)
                        - f.origin.x.max(from_frame.origin.x)
                }
            };
            if overlap <= 0.0 {
                continue; // 不在同一轴带上的不导航
            }
            let dist = match (dir, forward) {
                (Dir::Horizontal, true) => f.origin.x - (from_frame.origin.x + from_frame.size.width),
                (Dir::Horizontal, false) => from_frame.origin.x - (f.origin.x + f.size.width),
                (Dir::Vertical, true) => f.origin.y - (from_frame.origin.y + from_frame.size.height),
                (Dir::Vertical, false) => from_frame.origin.y - (f.origin.y + f.size.height),
            };
            if dist < -0.5 {
                continue; // 反方向
            }
            if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                best = Some((dist, v));
            }
        }
        if let Some((_, v)) = best
            && let Some(w) = self.window()
        {
            w.makeFirstResponder(Some(as_responder(&v)));
        }
    }

    /// ⌘[ / ⌘]（ghostty GOTO_SPLIT previous/next）：DFS 顺序循环切 pane。
    pub fn cycle_focus(&self, step: isize) {
        self.unzoom();
        let leaves = self.leaves();
        if leaves.len() < 2 {
            return;
        }
        let idx = leaves.iter().position(|v| {
            self.focused_leaf().is_some_and(|f| same_view(v, &f))
        });
        let next = match (idx, step) {
            (Some(i), 1) => (i + 1) % leaves.len(),
            (Some(i), -1) => (i + leaves.len() - 1) % leaves.len(),
            (None, _) => 0,
            _ => 0,
        };
        if let Some(w) = self.window() {
            w.makeFirstResponder(Some(as_responder(&leaves[next])));
        }
    }

    // ---- zoom（⌘⇧Enter：放大焦点 pane / 还原；无分屏 = 窗口 zoom）----

    /// ⌘⇧Enter 入口（菜单动作 / ghostty TOGGLE_SPLIT_ZOOM action / 取证
    /// 钩子同途）：按当前布局态分派（zoom_decision 状态机）。
    pub fn toggle_zoom(&self) {
        let target = self
            .focused_leaf()
            .or_else(|| self.leaves().first().cloned());
        match zoom_decision(self.leaf_count(), self.is_zoomed(), target.is_some()) {
            ZoomDecision::WindowZoom => {
                // 无分屏：等价窗口 zoom（最大化非全屏，NSWindow 原生）。
                if let Some(w) = self.window() {
                    w.zoom(None);
                }
            }
            ZoomDecision::ZoomFocused => {
                if let Some(t) = target {
                    self.zoom_leaf(&t);
                }
            }
            ZoomDecision::Restore => {
                self.unzoom();
            }
            ZoomDecision::None => {}
        }
    }

    /// 放大焦点 pane（已是放大态 = 无操作）。取证钩子 "zoom" 同途。
    pub fn zoom_focused(&self) {
        if self.ivars().zoomed.borrow().is_some() {
            return;
        }
        let target = self
            .focused_leaf()
            .or_else(|| self.leaves().first().cloned());
        if let Some(t) = target {
            self.zoom_leaf(&t);
        }
    }

    /// 还原布局（未放大 = 无操作）。布局树/比例原样未动，relayout 即回。
    pub fn unzoom(&self) {
        if self.ivars().zoomed.borrow_mut().take().is_some() {
            for v in self.leaves() {
                v.setHidden(false);
                v.set_surface_occlusion(true);
            }
            for d in self.ivars().dividers.borrow().values() {
                d.setHidden(false);
            }
            self.relayout();
        }
    }

    pub fn is_zoomed(&self) -> bool {
        self.ivars().zoomed.borrow().is_some()
    }

    /// 放大叶子的 pane id（E2E 取证用）。
    pub fn zoomed_pane_id(&self) -> Option<u32> {
        self.ivars().zoomed.borrow().as_ref().map(|v| v.pane_id())
    }

    /// 放大一个叶子：其余叶子 + 分隔条隐藏（不销毁：surface 常活，数据
    /// 继续喂不丢，隐藏面 set_occlusion(false) 停画），布局树/比例不动
    /// ——放大叶子由 relayout 按 zoom 态拿整窗 bounds（隐藏叶子不
    /// setFrame：surface 网格尺寸保持分屏态，还原即正确显示）。
    fn zoom_leaf(&self, view: &SurfaceHostView) {
        for v in self.leaves() {
            if !same_view(&v, view) {
                v.setHidden(true);
                v.set_surface_occlusion(false);
            }
        }
        for d in self.ivars().dividers.borrow().values() {
            d.setHidden(true);
        }
        *self.ivars().zoomed.borrow_mut() = Some(view.retain());
        self.relayout();
        // 放大面确保持焦（其它面已隐藏，不可夺焦）。
        if let Some(w) = self.window() {
            w.makeFirstResponder(Some(as_responder(view)));
        }
    }

    /// 取证：容器 zoom 态 + 各叶子（pane id/隐藏/缓存 frame/网格尺寸/
    /// 最下文本行）+ 窗口态的 JSON 快照（E2E 断言布局与内容用；v1 同款）。
    pub fn zoom_state_json(&self) -> String {
        let zoomed_pane = self.zoomed_pane_id();
        let mut s = String::from("{\"zoomed\":");
        s.push_str(if zoomed_pane.is_some() { "true" } else { "false" });
        match zoomed_pane {
            Some(p) => s.push_str(&format!(",\"zoomed_pane\":{p}")),
            None => s.push_str(",\"zoomed_pane\":null"),
        }
        s.push_str(",\"leaves\":[");
        let mut first = true;
        for (v, f) in self.leaves_with_frames() {
            if !first {
                s.push(',');
            }
            first = false;
            let (cols, rows) = v.grid_size();
            s.push_str(&format!(
                "{{\"pane\":{},\"hidden\":{},\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1},\"cols\":{},\"rows\":{},\"last\":\"{}\"}}",
                v.pane_id(),
                v.isHidden(),
                f.origin.x,
                f.origin.y,
                f.size.width,
                f.size.height,
                cols,
                rows,
                json_escape(&v.last_text_line()),
            ));
        }
        s.push(']');
        if let Some(w) = self.window() {
            let fr = w.frame();
            let fullscreen = w.styleMask().contains(NSWindowStyleMask::FullScreen);
            s.push_str(&format!(
                ",\"window\":{{\"zoomed\":{},\"fullscreen\":{},\"x\":{:.1},\"y\":{:.1},\"w\":{:.1},\"h\":{:.1}}}",
                w.isZoomed(),
                fullscreen,
                fr.origin.x,
                fr.origin.y,
                fr.size.width,
                fr.size.height,
            ));
        }
        s.push('}');
        s
    }

    // ---- 内部 ----

    fn contains(&self, view: &SurfaceHostView) -> bool {
        let tree = self.ivars().tree.borrow();
        node_contains(tree.as_ref(), view)
    }

    fn take_tree(&self) -> Node {
        self.ivars()
            .tree
            .borrow_mut()
            .take()
            .expect("pane tree always present")
    }

    fn set_tree_and_layout(&self, tree: Node) {
        *self.ivars().tree.borrow_mut() = Some(tree);
        self.relayout();
    }

    fn leaves_with_frames(&self) -> Vec<(Retained<SurfaceHostView>, NSRect)> {
        let tree = self.ivars().tree.borrow();
        let mut out = Vec::new();
        collect_leaves_with_frames(tree.as_ref(), &mut out);
        out
    }

    /// 递归布局：容器 bounds → 子树 rect（叶子 setFrame → resize 全链）。
    pub fn relayout(&self) {
        let bounds = self.bounds();
        let zoomed_ptr = self
            .ivars()
            .zoomed
            .borrow()
            .as_ref()
            .map(|z| &**z as *const SurfaceHostView);
        {
            let mut tree = self.ivars().tree.borrow_mut();
            if let Some(node) = tree.as_mut() {
                layout_node(node, bounds, &self.ivars().dividers, zoomed_ptr);
            }
        }
    }

    /// 拖拽回调（DividerView → 容器）：更新 split ratio 并重排。
    fn set_ratio(&self, split_id: u64, ratio: f64) {
        let ratio = ratio.clamp(RATIO_MIN, RATIO_MAX);
        {
            let mut tree = self.ivars().tree.borrow_mut();
            if let Some(node) = tree.as_mut() {
                set_node_ratio(node, split_id, ratio);
            }
        }
        self.relayout();
    }

    /// 拖拽回调：取 split 的缓存 frame（比例换算用）。
    fn split_frame(&self, split_id: u64) -> Option<NSRect> {
        let tree = self.ivars().tree.borrow();
        node_split_frame(tree.as_ref(), split_id)
    }

    /// EQUALIZE_SPLITS（ghostty action / ⌘⌃= 默认绑定）：全部 split 比例
    /// 归 0.5。
    pub fn equalize(&self) {
        self.unzoom();
        {
            let mut tree = self.ivars().tree.borrow_mut();
            if let Some(node) = tree.as_mut() {
                set_all_ratios(node, 0.5);
            }
        }
        self.relayout();
    }

    /// RESIZE_SPLIT（ghostty ⌘⌃方向键）：把焦点叶最近的可调分隔条沿
    /// 方向平移 amount px（左右 = Horizontal split，上下 = Vertical）。
    /// 方向语义 = 分隔条移动方向（right/down 增大 first 占比）。
    pub fn resize_split(&self, direction: ResizeDir, amount_px: f64) {
        self.unzoom();
        let Some(target) = self
            .focused_leaf()
            .or_else(|| self.leaves().first().cloned())
        else {
            return;
        };
        let (want_dir, positive): (Dir, bool) = match direction {
            ResizeDir::Up => (Dir::Vertical, false),
            ResizeDir::Down => (Dir::Vertical, true),
            ResizeDir::Left => (Dir::Horizontal, false),
            ResizeDir::Right => (Dir::Horizontal, true),
        };
        // 找 target 所在、方向匹配的最近祖先 split。
        let mut tree = self.ivars().tree.borrow_mut();
        let Some(node) = tree.as_mut() else { return };
        let found = find_ancestor_split(node, &target, want_dir);
        if let Some((id, ratio, frame)) = found {
            let axis_len = match want_dir {
                Dir::Horizontal => frame.size.width,
                Dir::Vertical => frame.size.height,
            };
            if axis_len <= 1.0 {
                return;
            }
            let delta = amount_px / axis_len;
            let new_ratio = (ratio + if positive { delta } else { -delta })
                .clamp(RATIO_MIN, RATIO_MAX);
            set_node_ratio(node, id, new_ratio);
            drop(tree);
            self.relayout();
        }
    }

}

/// RESIZE_SPLIT 方向（ghostty_action_resize_split_direction_e 的 Rust 面）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeDir {
    Up,
    Down,
    Left,
    Right,
}

fn same_view(a: &SurfaceHostView, b: &SurfaceHostView) -> bool {
    std::ptr::eq(
        a as *const SurfaceHostView,
        b as *const SurfaceHostView,
    )
}

// ---------------------------------------------------------------------------
// 树操作（自由函数：Node 不可 Clone 的递归重排；v1 原样 + before 插入）
// ---------------------------------------------------------------------------

fn collect_leaves(node: Option<&Node>, out: &mut Vec<Retained<SurfaceHostView>>) {
    match node {
        Some(Node::Leaf { view, .. }) => out.push(view.clone()),
        Some(Node::Split { first, second, .. }) => {
            collect_leaves(Some(first), out);
            collect_leaves(Some(second), out);
        }
        None => {}
    }
}

fn dump_node(node: Option<&Node>) -> LayoutNode {
    match node {
        Some(Node::Leaf { view, .. }) => LayoutNode::Leaf {
            // OSC-7 优先；没有 shell 集成（如 command= 直启非交互程序）时
            // 兜底读前台进程真实 cwd——与 pane 快照（cwd_for_view）同口径，
            // 否则恢复后 cwd 丢失，agent-restore 的槽位匹配永远等不到。
            pwd: view.ivars().pwd.borrow().clone().or_else(|| {
                let fallback = crate::plugins::cwd_for_view(view);
                (!fallback.is_empty()).then_some(fallback)
            }),
        },
        Some(Node::Split {
            dir,
            ratio,
            first,
            second,
            ..
        }) => LayoutNode::Split {
            dir: match dir {
                Dir::Horizontal => "h".into(),
                Dir::Vertical => "v".into(),
            },
            ratio: *ratio,
            first: Box::new(dump_node(Some(first))),
            second: Box::new(dump_node(Some(second))),
        },
        None => LayoutNode::Leaf { pwd: None },
    }
}

fn collect_pwds(dump: &LayoutNode) -> Vec<Option<String>> {
    let mut out = Vec::new();
    fn walk(n: &LayoutNode, out: &mut Vec<Option<String>>) {
        match n {
            LayoutNode::Leaf { pwd } => out.push(pwd.clone()),
            LayoutNode::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    walk(dump, &mut out);
    out
}

fn materialize(container: &PaneContainer, dump: &LayoutNode, mtm: MainThreadMarker) -> Node {
    match dump {
        LayoutNode::Leaf { .. } => {
            let view = SurfaceHostView::new(mtm);
            container.addSubview(&view);
            Node::Leaf {
                view,
                frame: NSRect::ZERO,
            }
        }
        LayoutNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let id = container.ivars().next_id.get();
            container.ivars().next_id.set(id + 1);
            let divider = DividerView::new(mtm, id);
            container.addSubview(&divider);
            container.ivars().dividers.borrow_mut().insert(id, divider);
            Node::Split {
                dir: if dir == "v" {
                    Dir::Vertical
                } else {
                    Dir::Horizontal
                },
                id,
                ratio: (*ratio).clamp(RATIO_MIN, RATIO_MAX),
                first: Box::new(materialize(container, first, mtm)),
                second: Box::new(materialize(container, second, mtm)),
                frame: NSRect::ZERO,
            }
        }
    }
}

fn collect_leaves_with_frames(
    node: Option<&Node>,
    out: &mut Vec<(Retained<SurfaceHostView>, NSRect)>,
) {
    match node {
        Some(Node::Leaf { view, frame }) => out.push((view.clone(), *frame)),
        Some(Node::Split { first, second, .. }) => {
            collect_leaves_with_frames(Some(first), out);
            collect_leaves_with_frames(Some(second), out);
        }
        None => {}
    }
}

fn node_contains(node: Option<&Node>, view: &SurfaceHostView) -> bool {
    match node {
        Some(Node::Leaf { view: v, .. }) => same_view(v, view),
        Some(Node::Split { first, second, .. }) => {
            node_contains(Some(first), view) || node_contains(Some(second), view)
        }
        None => false,
    }
}

#[allow(dead_code)] // 焦点环关掉后布局 dump 仍可能用
fn node_leaf_frame(node: Option<&Node>, view: &SurfaceHostView) -> Option<NSRect> {
    match node {
        Some(Node::Leaf { view: v, frame }) => {
            if same_view(v, view) {
                Some(*frame)
            } else {
                None
            }
        }
        Some(Node::Split { first, second, .. }) => node_leaf_frame(Some(first), view)
            .or_else(|| node_leaf_frame(Some(second), view)),
        None => None,
    }
}

fn node_split_frame(node: Option<&Node>, id: u64) -> Option<NSRect> {
    match node {
        Some(Node::Split { id: sid, frame, first, second, .. }) => {
            if *sid == id {
                Some(*frame)
            } else {
                node_split_frame(Some(first), id).or_else(|| node_split_frame(Some(second), id))
            }
        }
        _ => None,
    }
}

fn set_node_ratio(node: &mut Node, id: u64, ratio: f64) -> bool {
    match node {
        Node::Leaf { .. } => false,
        Node::Split { id: sid, ratio: r, first, second, .. } => {
            if *sid == id {
                *r = ratio;
                true
            } else {
                set_node_ratio(first, id, ratio) || set_node_ratio(second, id, ratio)
            }
        }
    }
}

fn set_all_ratios(node: &mut Node, ratio: f64) {
    match node {
        Node::Leaf { .. } => {}
        Node::Split { ratio: r, first, second, .. } => {
            *r = ratio;
            set_all_ratios(first, ratio);
            set_all_ratios(second, ratio);
        }
    }
}

/// target 最近的方向匹配祖先 split（id, 当前 ratio, frame）。
fn find_ancestor_split(
    node: &mut Node,
    target: &SurfaceHostView,
    dir: Dir,
) -> Option<(u64, f64, NSRect)> {
    match node {
        // 叶子无祖先 split 可调（containment 由父 Split 预判）。
        Node::Leaf { .. } => None,
        Node::Split { dir: d, id, ratio, first, second, frame } => {
            let in_first = node_contains(Some(first), target);
            let in_second = node_contains(Some(second), target);
            if !in_first && !in_second {
                return None;
            }
            // 先问更近的祖先（子树深处优先）。
            let child = if in_first { first.as_mut() } else { second.as_mut() };
            if let Some(found) = find_ancestor_split(child, target, dir) {
                return Some(found);
            }
            if *d == dir {
                return Some((*id, *ratio, *frame));
            }
            None
        }
    }
}

fn layout_node(
    node: &mut Node,
    rect: NSRect,
    dividers: &RefCell<HashMap<u64, Retained<DividerView>>>,
    zoomed: Option<*const SurfaceHostView>,
) {
    match node {
        Node::Leaf { view, frame } => {
            if let Some(z) = zoomed {
                // 放大态——只有放大叶子落 bounds；其余叶子隐藏且不
                // setFrame（surface 网格尺寸冻结在分屏态，数据照喂）。
                if std::ptr::eq(&**view as *const SurfaceHostView, z) {
                    if view.frame() != rect {
                        view.setFrame(rect);
                    }
                    *frame = rect;
                }
                return;
            }
            if view.frame() != rect {
                view.setFrame(rect);
            }
            *frame = rect;
        }
        Node::Split { dir, id, ratio, first, second, frame } => {
            *frame = rect;
            if zoomed.is_some() {
                // 放大态：不切分——两侧子树都拿整 rect，放大叶子落到整窗
                // bounds，隐藏叶子自行跳过；分隔条已隐藏，几何冻结。
                layout_node(first, rect, dividers, zoomed);
                layout_node(second, rect, dividers, zoomed);
                return;
            }
            let (ra, rb, rdiv) = split_rects(rect, *dir, *ratio);
            let divider_view = dividers.borrow().get(id).cloned();
            if let Some(d) = divider_view
                && d.frame() != rdiv
            {
                d.setFrame(rdiv);
            }
            layout_node(first, ra, dividers, zoomed);
            layout_node(second, rb, dividers, zoomed);
        }
    }
}

/// 在 `target` 叶子旁插入新 pane。`before`=新 pane 进 first 位（左/上），
/// 否则 second 位（右/下，⌘D/⌘⇧D）。命中返回 true（树已改）。
fn insert_beside(
    node: &mut Node,
    target: &SurfaceHostView,
    new_view: Retained<SurfaceHostView>,
    dir: Dir,
    id: u64,
    before: bool,
) -> bool {
    let is_target = matches!(node, Node::Leaf { view, .. } if same_view(view, target));
    if is_target {
        // 用新叶子暂占本位，取出旧叶子，再组 Split 放回。
        let old = std::mem::replace(
            node,
            Node::Leaf {
                view: new_view.clone(),
                frame: NSRect::ZERO,
            },
        );
        let Node::Leaf {
            view: old_view,
            frame: old_frame,
        } = old
        else {
            unreachable!("just matched Leaf");
        };
        let (first_view, first_frame, second_view) = if before {
            (
                new_view.clone(),
                NSRect::ZERO,
                old_view,
            )
        } else {
            (old_view, old_frame, new_view.clone())
        };
        *node = Node::Split {
            dir,
            id,
            ratio: 0.5,
            first: Box::new(Node::Leaf {
                view: first_view,
                frame: first_frame,
            }),
            second: Box::new(Node::Leaf {
                view: second_view,
                frame: NSRect::ZERO,
            }),
            frame: NSRect::ZERO,
        };
        return true;
    }
    match node {
        Node::Split { first, second, .. } => {
            // 先试 first（clone 仅多一次 retain，未命中即释放）。
            insert_beside(first, target, new_view.clone(), dir, id, before)
                || insert_beside(second, target, new_view, dir, id, before)
        }
        Node::Leaf { .. } => false,
    }
}

/// 从树里摘掉 `target` 叶子；父 split 塌缩为另一侧子树（其 id 记入
/// `dropped`，调用方移除对应分隔条视图）。None = 整树就是该叶子。
fn remove_leaf(node: Node, target: &SurfaceHostView, dropped: &mut Vec<u64>) -> Option<Node> {
    match node {
        Node::Leaf { view, .. } => {
            if same_view(&view, target) {
                None
            } else {
                Some(Node::Leaf {
                    view,
                    frame: NSRect::ZERO,
                })
            }
        }
        Node::Split { id, first, second, dir, ratio, .. } => {
            match remove_leaf(*first, target, dropped) {
                None => {
                    dropped.push(id);
                    Some(*second)
                }
                Some(f) => match remove_leaf(*second, target, dropped) {
                    None => {
                        dropped.push(id);
                        Some(f)
                    }
                    Some(s) => Some(Node::Split {
                        dir,
                        id,
                        ratio,
                        first: Box::new(f),
                        second: Box::new(s),
                        frame: NSRect::ZERO,
                    }),
                },
            }
        }
    }
}

/// 手工 JSON 字符串转义（v1 原样）。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// DividerView：可拖分隔条
// ---------------------------------------------------------------------------

pub struct DividerIvars {
    pub split_id: u64,
    pub drag_ratio: Cell<f64>,
}

define_class!(
    // SAFETY: NSView 子类化无强约束方法；鼠标事件先走 super。
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DividerIvars]
    pub struct DividerView;

    impl DividerView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let b = self.bounds();
            crate::host::bg_color().set();
            objc2_app_kit::NSRectFill(b);
            crate::host::divider_color().set();
            let line = if b.size.width < b.size.height {
                // 竖分隔条：居中 1px 竖线。
                NSRect {
                    origin: NSPoint {
                        x: (b.size.width - 1.0) / 2.0,
                        y: 0.0,
                    },
                    size: NSSize {
                        width: 1.0,
                        height: b.size.height,
                    },
                }
            } else {
                NSRect {
                    origin: NSPoint {
                        x: 0.0,
                        y: (b.size.height - 1.0) / 2.0,
                    },
                    size: NSSize {
                        width: b.size.width,
                        height: 1.0,
                    },
                }
            };
            objc2_app_kit::NSRectFill(line);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            // 记住按下时的 ratio，拖拽以事件流累计。
            self.ivars().drag_ratio.set(0.0);
            self.update_ratio(&_event.locationInWindow());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.update_ratio(&event.locationInWindow());
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {}
    }
);

impl DividerView {
    fn new(mtm: MainThreadMarker, split_id: u64) -> Retained<Self> {
        let frame = NSRect::ZERO;
        let this = DividerView::alloc(mtm).set_ivars(DividerIvars {
            split_id,
            drag_ratio: Cell::new(0.0),
        });
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// 所属容器 = 本视图的 superview（布局保证分隔条只挂在容器上；
    /// 寿命纪律见 downcast_container）。
    fn container(&self) -> Option<&'static PaneContainer> {
        // SAFETY: superview 仅读引用（视图层级在主线程稳定）。
        let superview = unsafe { self.superview() }?;
        if !is_container(&superview) {
            return None;
        }
        Some(downcast_container(&superview))
    }

    /// 窗口坐标 → 容器坐标 → 相对 split frame 的比例 → set_ratio。
    fn update_ratio(&self, loc_window: &NSPoint) {
        let Some(container) = self.container() else {
            return;
        };
        let p = container.convertPoint_fromView(*loc_window, None);
        let Some(f) = container.split_frame(self.ivars().split_id) else {
            return;
        };
        // 从分隔条自身取方向（宽 < 高 = 竖条 = Horizontal split）。
        let b = self.bounds();
        let ratio = if b.size.width < b.size.height {
            (p.x - f.origin.x) / f.size.width.max(1.0)
        } else {
            (p.y - f.origin.y) / f.size.height.max(1.0)
        };
        container.set_ratio(self.ivars().split_id, ratio);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width: w, height: h },
        }
    }

    #[test]
    fn zoom_decision_state_machine() {
        // X3 布局态机（纯逻辑）：
        // 无分屏（单面/防御性零面）→ 窗口 zoom（最大化非全屏）。
        assert_eq!(zoom_decision(1, false, true), ZoomDecision::WindowZoom);
        assert_eq!(zoom_decision(1, true, true), ZoomDecision::WindowZoom);
        assert_eq!(zoom_decision(0, false, false), ZoomDecision::WindowZoom);
        // 有分屏 + 未放大 → 放大焦点面。
        assert_eq!(zoom_decision(2, false, true), ZoomDecision::ZoomFocused);
        assert_eq!(zoom_decision(3, false, true), ZoomDecision::ZoomFocused);
        // 有分屏 + 已放大 → 还原（优先于焦点判断：焦点丢了也该还原）。
        assert_eq!(zoom_decision(2, true, true), ZoomDecision::Restore);
        assert_eq!(zoom_decision(3, true, false), ZoomDecision::Restore);
        // 有分屏但无可用目标（异常态）→ 无操作，不碰窗口 zoom。
        assert_eq!(zoom_decision(2, false, false), ZoomDecision::None);
    }

    #[test]
    fn json_escape_minimal() {
        // 防御：引号/反斜杠/控制字符不破坏 JSON 结构。
        assert_eq!(json_escape("tick 42"), "tick 42");
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(json_escape("\u{1}"), " ");
    }

    #[test]
    fn split_rects_sum_to_parent() {
        let parent = rect(10.0, 20.0, 400.0, 300.0);
        for dir in [Dir::Horizontal, Dir::Vertical] {
            for ratio in [0.15, 0.3, 0.5, 0.7, 0.85, -1.0, 2.0] {
                let (a, b, d) = split_rects(parent, dir, ratio);
                let len = |r: NSRect, horizontal: bool| {
                    if horizontal {
                        r.size.width
                    } else {
                        r.size.height
                    }
                };
                let axis = if dir == Dir::Horizontal {
                    parent.size.width
                } else {
                    parent.size.height
                };
                let total = len(a, dir == Dir::Horizontal)
                    + len(b, dir == Dir::Horizontal)
                    + len(d, dir == Dir::Horizontal);
                assert!(
                    (total - axis).abs() < 1e-9,
                    "dir={dir:?} ratio={ratio}: {total} != {axis}"
                );
                // 垂直方向的尺寸原样透传。
                match dir {
                    Dir::Horizontal => {
                        assert_eq!(a.size.height, 300.0);
                        assert_eq!(b.size.height, 300.0);
                        // second 在 first 右侧、divider 居中。
                        assert!(b.origin.x >= a.origin.x + a.size.width + DIVIDER - 1e-9);
                        assert_eq!(d.size.width, DIVIDER);
                    }
                    Dir::Vertical => {
                        assert_eq!(a.size.width, 400.0);
                        assert_eq!(b.size.width, 400.0);
                        assert!(b.origin.y >= a.origin.y + a.size.height + DIVIDER - 1e-9);
                        assert_eq!(d.size.height, DIVIDER);
                    }
                }
                // 越界 ratio 被夹回 [0.15, 0.85]。
                let a_frac = len(a, dir == Dir::Horizontal) + DIVIDER / 2.0;
                let frac = a_frac / axis;
                assert!((0.14..=0.86).contains(&frac), "frac={frac}");
            }
        }
    }
}
