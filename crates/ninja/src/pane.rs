//! p2 分屏容器：一个窗口 = 一个 [`PaneContainer`]（NSView），持一棵
//! pane 树（叶子 = `TerminalView`，内部节点 = 带 ratio 的二叉 split）。
//!
//! 布局：`relayout` 递归把容器 bounds 按 ratio 切给子树，叶子
//! `setFrame` → `setFrameSize` → `grid_changed` → vt resize + PTY
//! `TIOCSWINSZ`（复用 p1 的 resize 链路）。分隔条是子 NSView（可拖调
//! ratio）。焦点：叶子 `becomeFirstResponder`/点击夺焦（AppKit 默认），
//! 焦点环（CALayer 边框）叠在最上层标示当前 pane。
//!
//! 菜单动作（split/关 pane/焦点导航）实现在本类上：first responder
//! （TerminalView）不接的动作沿响应链冒泡到 superview = 本容器。

#![allow(non_snake_case)] // ObjC selector 方法名
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSColor, NSEvent, NSResponder, NSView};
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::CALayer;

use crate::config::Config;
use crate::view::TerminalView;

/// 分隔条厚度（points；命中区即此厚度）。
pub const DIVIDER: f64 = 5.0;
/// ratio 夹取范围：两侧叶子各保最小占比。
const RATIO_MIN: f64 = 0.15;
const RATIO_MAX: f64 = 0.85;
/// 焦点环边框厚度（points）。
const RING_BORDER: f64 = 1.5;

/// 分屏方向：Horizontal = 左右排（⌘D 右分），Vertical = 上下排（⌘⇧D 下分）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Horizontal,
    Vertical,
}

/// pane 树。frame 由 `relayout` 缓存（焦点导航/拖拽换算用）。
enum Node {
    Leaf {
        view: Retained<TerminalView>,
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
/// 纯函数（布局单测用）。
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
    config: Config,
    tree: RefCell<Option<Node>>,
    next_id: Cell<u64>,
    dividers: RefCell<HashMap<u64, Retained<DividerView>>>,
    ring: RefCell<Option<Retained<FocusRingView>>>,
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
            true // 左上原点，与 TerminalView 一致
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false // 焦点在叶子终端面，不在容器
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            // SAFETY: 标准 super 调用。
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.relayout();
        }

        /// 背景填黑：分隔条缝隙/角落不露白（终端底色一致）。
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::blackColor().set();
            objc2_app_kit::NSRectFill(self.bounds());
        }

        // ---- 菜单动作（first responder 不接 → 冒泡到本容器）----

        #[unsafe(method(ninjaSplitRight:))]
        fn split_right(&self, _sender: Option<&AnyObject>) {
            self.split_focused(Dir::Horizontal);
        }

        #[unsafe(method(ninjaSplitDown:))]
        fn split_down(&self, _sender: Option<&AnyObject>) {
            self.split_focused(Dir::Vertical);
        }

        #[unsafe(method(ninjaClosePane:))]
        fn close_pane(&self, _sender: Option<&AnyObject>) {
            if let Some(view) = self.focused_leaf() {
                self.close_leaf(&view);
            }
        }

        #[unsafe(method(ninjaFocusLeft:))]
        fn focus_left(&self, _sender: Option<&AnyObject>) {
            self.focus_dir(Dir::Horizontal, false);
        }

        #[unsafe(method(ninjaFocusRight:))]
        fn focus_right(&self, _sender: Option<&AnyObject>) {
            self.focus_dir(Dir::Horizontal, true);
        }

        #[unsafe(method(ninjaFocusUp:))]
        fn focus_up(&self, _sender: Option<&AnyObject>) {
            self.focus_dir(Dir::Vertical, false);
        }

        #[unsafe(method(ninjaFocusDown:))]
        fn focus_down(&self, _sender: Option<&AnyObject>) {
            self.focus_dir(Dir::Vertical, true);
        }

        #[unsafe(method(ninjaPrevPane:))]
        fn prev_pane(&self, _sender: Option<&AnyObject>) {
            self.cycle_focus(-1);
        }

        #[unsafe(method(ninjaNextPane:))]
        fn next_pane(&self, _sender: Option<&AnyObject>) {
            self.cycle_focus(1);
        }
    }
);

// ---------------------------------------------------------------------------
// Rust 接口
// ---------------------------------------------------------------------------

/// TerminalView（NSView 子类）→ NSResponder 引用（makeFirstResponder 用）。
fn as_responder(v: &TerminalView) -> &NSResponder {
    v.as_super().as_super()
}

impl PaneContainer {
    /// 建容器 + 首个 pane。frame 先按默认 80x24 cell（TerminalView 初值）。
    pub fn new(mtm: MainThreadMarker, config: &Config) -> Retained<Self> {
        let first = TerminalView::new(mtm, config);
        let frame = first.frame();
        let ring = FocusRingView::new(mtm, config.cursor);

        let this = PaneContainer::alloc(mtm).set_ivars(Ivars {
            config: config.clone(),
            tree: RefCell::new(None),
            next_id: Cell::new(1),
            dividers: RefCell::new(HashMap::new()),
            ring: RefCell::new(Some(ring)),
        });
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        let view: Retained<PaneContainer> =
            unsafe { msg_send![super(this), initWithFrame: frame] };
        view.addSubview(&first);
        *view.ivars().tree.borrow_mut() = Some(Node::Leaf {
            view: first,
            frame,
        });
        view.relayout();
        view
    }

    /// 当前焦点叶子（first responder 是本容器的某个 TerminalView）。
    pub fn focused_leaf(&self) -> Option<Retained<TerminalView>> {
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
    pub fn leaves(&self) -> Vec<Retained<TerminalView>> {
        let tree = self.ivars().tree.borrow();
        let mut out = Vec::new();
        collect_leaves(tree.as_ref(), &mut out);
        out
    }

    pub fn contains(&self, view: &TerminalView) -> bool {
        let tree = self.ivars().tree.borrow();
        node_contains(tree.as_ref(), view)
    }

    pub fn leaf_count(&self) -> usize {
        self.leaves().len()
    }

    /// ⌘D/⌘⇧D：在焦点叶子旁插一个新 pane（新 pane 夺焦）。
    pub fn split_focused(&self, dir: Dir) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let target = self
            .focused_leaf()
            .or_else(|| self.leaves().first().cloned());
        let Some(target) = target else { return };

        let new_view = TerminalView::new(mtm, &self.ivars().config);
        self.addSubview(&new_view);

        let id = self.ivars().next_id.get();
        self.ivars().next_id.set(id + 1);
        let divider = DividerView::new(mtm, id);
        self.addSubview(&divider);
        self.ivars().dividers.borrow_mut().insert(id, divider);

        let mut tree = self.take_tree();
        if insert_beside(&mut tree, &target, new_view.clone(), dir, id) {
            self.set_tree_and_layout(tree);
        } else {
            // 不应发生（target 来自本容器的树）；防御性回收刚加的子视图。
            self.set_tree_and_layout(tree);
            new_view.shutdown();
            new_view.removeFromSuperview();
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

    /// 关一个 pane：树里只剩它 → 关窗；否则从树里拆掉（连带父 split 的
    /// 分隔条）。`view` 必须属于本容器（调用方保证）。
    pub fn close_leaf(&self, view: &TerminalView) {
        if self.leaf_count() <= 1 {
            // 最后一个 pane：整窗关（windowWillClose 走 shutdown_all）。
            if let Some(w) = self.window() {
                w.performClose(None);
            }
            return;
        }
        // 先把焦点从待拆 pane 挪走：NSWindow 的 firstResponder 不额外
        // 持引用，先 resign 再释放视图，否则窗口后续事件路径触已释放
        // 对象（p2 实测关 pane/关窗 SEGFAULT 根因）。
        if let Some(w) = self.window() {
            let removing_is_focused = self.focused_leaf().is_some_and(|f| {
                std::ptr::eq(&*f as *const TerminalView, view as *const TerminalView)
            });
            if removing_is_focused {
                let other = self.leaves().into_iter().find(|v| {
                    !std::ptr::eq(&**v as *const TerminalView, view as *const TerminalView)
                });
                if let Some(o) = other {
                    w.makeFirstResponder(Some(as_responder(&o)));
                }
            }
        }
        view.shutdown();
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
        if self.focused_leaf().is_none() {
            if let (Some(first), Some(w)) = (leaves.first(), self.window()) {
                w.makeFirstResponder(Some(as_responder(first)));
            }
        }
    }

    /// 关窗/退出前收尾全部 pane（幂等；EOF 先到过的叶子再走一遍无害）。
    /// 关窗/退出前收尾全部 pane（幂等；EOF 先到过的叶子再走一遍无害）。
    /// 只拆资源（PTY/timer/source/renderer），**不**碰视图层级：
    /// windowWillClose 期间 AppKit 的收尾（含 firstResponder resign）
    /// 还会触碰子视图，过早 removeFromSuperview/释放会留悬空指针
    ///（p2 实测关窗 SEGFAULT）。视图随窗口 contentView 释放自然亡。
    pub fn shutdown_all(&self) {
        let leaves = self.leaves();
        for v in leaves {
            v.shutdown();
        }
        // 焦点环停用：隐藏即可，不摘视图（同上）。
        if let Some(ring) = self.ivars().ring.borrow().clone() {
            ring.setHidden(true);
        }
    }

    /// 焦点方向导航：按叶子 frame 找相邻重叠面上最近的那个。
    fn focus_dir(&self, dir: Dir, forward: bool) {
        let Some(from) = self.focused_leaf() else { return };
        let Some((_, from_frame)) = self.leaves_with_frames().into_iter().find(|(v, _)| {
            std::ptr::eq(&**v as *const TerminalView, &*from as *const TerminalView)
        }) else {
            return;
        };
        let mut best: Option<(f64, Retained<TerminalView>)> = None;
        for (v, f) in self.leaves_with_frames() {
            if std::ptr::eq(&*v as *const TerminalView, &*from as *const TerminalView) {
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
        if let Some((_, v)) = best {
            if let Some(w) = self.window() {
                w.makeFirstResponder(Some(as_responder(&v)));
            }
        }
    }

    /// ⌘[ / ⌘]：DFS 顺序循环切 pane。
    fn cycle_focus(&self, step: isize) {
        let leaves = self.leaves();
        if leaves.len() < 2 {
            return;
        }
        let idx = leaves.iter().position(|v| {
            self.focused_leaf()
                .is_some_and(|f| std::ptr::eq(&**v as *const TerminalView, &*f as *const TerminalView))
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

    /// 焦点环同步（焦点变化 / 布局变化后调；view 的 become/resign 也经
    /// shell::update_focus_rings 走到这里）。
    pub fn sync_focus_ring(&self) {
        let Some(ring) = self.ivars().ring.borrow().clone() else {
            return;
        };
        // 环必须在最上层（新加的 pane 子视图会盖住它）。
        ring.removeFromSuperview();
        self.addSubview(&ring);
        let frame = self
            .focused_leaf()
            .and_then(|v| {
                let tree = self.ivars().tree.borrow();
                node_leaf_frame(tree.as_ref(), &v)
            })
            .unwrap_or(NSRect::ZERO);
        ring.setFrame(frame);
        ring.setHidden(frame.size.width <= 0.0 || frame.size.height <= 0.0);
    }

    // ---- 内部 ----

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

    fn leaves_with_frames(&self) -> Vec<(Retained<TerminalView>, NSRect)> {
        let tree = self.ivars().tree.borrow();
        let mut out = Vec::new();
        collect_leaves_with_frames(tree.as_ref(), &mut out);
        out
    }

    /// 递归布局：容器 bounds → 子树 rect（叶子 setFrame 触发 p1 resize 链）。
    /// 同步的 grid_changed→render_now（setFrameSize 内）只保证 drawable
    /// 已呈现；分屏/拖分隔条时该绘制发生在我们自己的布局调用栈里，
    /// 图层几何还没提交，可能不上屏——所以布局尾部统一补
    /// `setNeedsDisplay`，把重画推迟到 AppKit 显示周期（drawRect 路径）。
    fn relayout(&self) {
        let bounds = self.bounds();
        let leaves;
        {
            let mut tree = self.ivars().tree.borrow_mut();
            if let Some(node) = tree.as_mut() {
                layout_node(node, bounds, &self.ivars().dividers);
            }
            leaves = {
                let mut v = Vec::new();
                collect_leaves(tree.as_ref(), &mut v);
                v
            };
        }
        for v in leaves {
            v.setNeedsDisplay(true);
        }
        self.sync_focus_ring();
    }

    /// 拖拽回调（DividerView → 容器）：更新 split ratio 并重排。
    fn set_ratio(&self, split_id: u64, ratio: f64) {
        let ratio = ratio.clamp(RATIO_MIN, RATIO_MAX);
        let mut tree = self.ivars().tree.borrow_mut();
        if let Some(node) = tree.as_mut() {
            set_node_ratio(node, split_id, ratio);
        }
        drop(tree);
        self.relayout();
    }

    /// 拖拽回调：取 split 的缓存 frame（比例换算用）。
    fn split_frame(&self, split_id: u64) -> Option<NSRect> {
        let tree = self.ivars().tree.borrow();
        node_split_frame(tree.as_ref(), split_id)
    }
}

// ---------------------------------------------------------------------------
// 树操作（自由函数：Node 不可 Clone 的递归重排）
// ---------------------------------------------------------------------------

fn collect_leaves(node: Option<&Node>, out: &mut Vec<Retained<TerminalView>>) {
    match node {
        Some(Node::Leaf { view, .. }) => out.push(view.clone()),
        Some(Node::Split { first, second, .. }) => {
            collect_leaves(Some(first), out);
            collect_leaves(Some(second), out);
        }
        None => {}
    }
}

fn collect_leaves_with_frames(node: Option<&Node>, out: &mut Vec<(Retained<TerminalView>, NSRect)>) {
    match node {
        Some(Node::Leaf { view, frame }) => out.push((view.clone(), *frame)),
        Some(Node::Split { first, second, .. }) => {
            collect_leaves_with_frames(Some(first), out);
            collect_leaves_with_frames(Some(second), out);
        }
        None => {}
    }
}

fn node_contains(node: Option<&Node>, view: &TerminalView) -> bool {
    match node {
        Some(Node::Leaf { view: v, .. }) => std::ptr::eq(&**v as *const TerminalView, view as *const TerminalView),
        Some(Node::Split { first, second, .. }) => {
            node_contains(Some(first), view) || node_contains(Some(second), view)
        }
        None => false,
    }
}

fn node_leaf_frame(node: Option<&Node>, view: &TerminalView) -> Option<NSRect> {
    match node {
        Some(Node::Leaf { view: v, frame }) => {
            if std::ptr::eq(&**v as *const TerminalView, view as *const TerminalView) {
                Some(*frame)
            } else {
                None
            }
        }
        Some(Node::Split { first, second, .. }) => {
            node_leaf_frame(Some(first), view).or_else(|| node_leaf_frame(Some(second), view))
        }
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

fn layout_node(node: &mut Node, rect: NSRect, dividers: &RefCell<HashMap<u64, Retained<DividerView>>>) {
    match node {
        Node::Leaf { view, frame } => {
            if view.frame() != rect {
                view.setFrame(rect);
            }
            *frame = rect;
        }
        Node::Split { dir, id, ratio, first, second, frame } => {
            *frame = rect;
            let (ra, rb, rdiv) = split_rects(rect, *dir, *ratio);
            let divider_view = dividers.borrow().get(id).cloned();
            if let Some(d) = divider_view {
                if d.frame() != rdiv {
                    d.setFrame(rdiv);
                }
            }
            layout_node(first, ra, dividers);
            layout_node(second, rb, dividers);
        }
    }
}

/// 在 `target` 叶子旁插入新 pane（新 pane 在 second 位：右分/下分）。
/// 命中返回 true（树已改）；树里没有 target 返回 false（树未动）。
fn insert_beside(
    node: &mut Node,
    target: &TerminalView,
    new_view: Retained<TerminalView>,
    dir: Dir,
    id: u64,
) -> bool {
    let is_target = matches!(node, Node::Leaf { view, .. }
        if std::ptr::eq(&**view as *const TerminalView, target as *const TerminalView));
    if is_target {
        // 用新叶子暂占本位，取出旧叶子，再组 Split 放回。
        let old = std::mem::replace(
            node,
            Node::Leaf {
                view: new_view.clone(),
                frame: NSRect::ZERO,
            },
        );
        let Node::Leaf { view: old_view, frame: old_frame } = old else {
            unreachable!("just matched Leaf");
        };
        *node = Node::Split {
            dir,
            id,
            ratio: 0.5,
            first: Box::new(Node::Leaf {
                view: old_view,
                frame: old_frame,
            }),
            second: Box::new(Node::Leaf {
                view: new_view,
                frame: NSRect::ZERO,
            }),
            frame: NSRect::ZERO,
        };
        return true;
    }
    match node {
        Node::Split { first, second, .. } => {
            // 先试 first（clone 仅多一次 retain，未命中即释放）。
            insert_beside(first, target, new_view.clone(), dir, id)
                || insert_beside(second, target, new_view, dir, id)
        }
        Node::Leaf { .. } => false,
    }
}

/// 从树里摘掉 `target` 叶子；父 split 塌缩为另一侧子树（其 id 记入
/// `dropped`，调用方移除对应分隔条视图）。返回 None = 整树就是该叶子。
fn remove_leaf(node: Node, target: &TerminalView, dropped: &mut Vec<u64>) -> Option<Node> {
    match node {
        Node::Leaf { view, .. } => {
            if std::ptr::eq(&*view as *const TerminalView, target as *const TerminalView) {
                None
            } else {
                Some(Node::Leaf { view, frame: NSRect::ZERO })
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
            NSColor::blackColor().set();
            objc2_app_kit::NSRectFill(b);
            NSColor::separatorColor().set();
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
            // 记住按下时的 ratio，拖拽以事件流累计（mouseDragged 逐次重算）。
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

    /// 所属容器 = 本视图的 superview（布局保证分隔条只挂在容器上）。
    fn container(&self) -> Option<&PaneContainer> {
        // SAFETY: superview 仅读引用（视图层级在主线程稳定）。
        let superview = unsafe { self.superview() }?;
        // SAFETY: isKindOfClass: 任意 NSObject 可查；通过后指针上转安全。
        let is_container: bool =
            unsafe { msg_send![&*superview, isKindOfClass: PaneContainer::class()] };
        if is_container {
            Some(unsafe { &*(std::ptr::from_ref(&*superview) as *const PaneContainer) })
        } else {
            None
        }
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

// ---------------------------------------------------------------------------
// FocusRingView：焦点 pane 的边框指示（最上层、不挡鼠标）
// ---------------------------------------------------------------------------

pub struct RingIvars;

define_class!(
    // SAFETY: NSView 子类化无强约束方法；hitTest 返回 nil 不参与命中。
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RingIvars]
    pub struct FocusRingView;

    impl FocusRingView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// 不挡鼠标：命中测试永远失败。
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }
    }
);

impl FocusRingView {
    fn new(mtm: MainThreadMarker, color: crate::term::Rgb) -> Retained<Self> {
        let frame = NSRect::ZERO;
        let this = FocusRingView::alloc(mtm).set_ivars(RingIvars);
        // SAFETY: super 的 initWithFrame:；ivars 已就位。
        let view: Retained<FocusRingView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        let layer = CALayer::new();
        layer.setBorderWidth(RING_BORDER);
        // SAFETY: 组件数组布局正确（CGColorCreate，sRGB 空间）。
        unsafe {
            if let Some(space) = objc2_core_graphics::CGColorSpace::new_device_rgb() {
                let comps: [f64; 4] = [
                    f64::from(color.0) / 255.0,
                    f64::from(color.1) / 255.0,
                    f64::from(color.2) / 255.0,
                    0.9,
                ];
                if let Some(c) = CGColor::new(Some(&*space), comps.as_ptr()) {
                    layer.setBorderColor(Some(c.as_ref()));
                }
            }
        }
        view.setLayer(Some(&layer));
        view
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
                // 分割轴总长（Horizontal=宽 400，Vertical=高 300）。
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
