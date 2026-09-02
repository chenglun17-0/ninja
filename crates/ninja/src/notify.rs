//! Ghostty 桌面通知适配器：OSC 9 / 777 / 99 → `UNUserNotification`。
//!
//! libghostty 已经解析 OSC 并回调 `GHOSTTY_ACTION_DESKTOP_NOTIFICATION`。
//! 本模块只做壳：授权、弹出、点通知聚焦发这条 OSC 的那块面。
//! 不进 ADE 协议，不出现插件名词。

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSResponder, NSWindow};
use objc2_foundation::{NSArray, NSDictionary, NSObject, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::host;
use crate::surface::{SurfaceHostView, as_responder};

const USERINFO_PANE: &str = "pane";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// 点通知后要聚焦的 pane；0 = 无。主线程 tick 排干（delegate 可能不在主线程）。
static PENDING_FOCUS: AtomicU32 = AtomicU32::new(0);
static DELIVERED: LazyLock<Mutex<HashMap<u32, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DELEGATE: Mutex<Option<Retained<NotifyDelegate>>> = Mutex::new(None);

define_class!(
    // SAFETY: 纯 NSObject 子类；UNUserNotificationCenter.delegate 是弱引用，
    // 由 DELEGATE 槽保活。系统可能在私有队列回调，故不用 MainThreadOnly。
    #[unsafe(super(NSObject))]
    #[name = "NinjaNotifyDelegate"]
    pub struct NotifyDelegate;

    unsafe impl NSObjectProtocol for NotifyDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotifyDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn userNotificationCenter_willPresentNotification_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let opts = if pane_from_notification(notification)
                .map(should_present_for_pane)
                .unwrap_or(false)
            {
                UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound
            } else {
                UNNotificationPresentationOptions::empty()
            };
            completion_handler.call((opts,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn userNotificationCenter_didReceiveNotificationResponse_withCompletionHandler(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let action = response.actionIdentifier();
            let default = unsafe { UNNotificationDefaultActionIdentifier };
            if action.isEqualToString(default)
                && let Some(pane) = pane_from_notification(&response.notification())
            {
                request_focus(pane);
            }
            forget_notification(&response.notification());
            completion_handler.call(());
        }
    }
);

/// 启动时挂 delegate（弱引用，须有人持有）。主线程调用。
pub fn install() {
    let delegate: Retained<NotifyDelegate> = unsafe { msg_send![NotifyDelegate::class(), new] };
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    if let Ok(mut slot) = DELEGATE.lock() {
        *slot = Some(delegate);
    }
}

/// OSC 桌面通知：title/body 来自 libghostty（指针仅回调内有效，这里立刻拷走）。
pub fn show(view: &SurfaceHostView, title: &str, body: &str) {
    let pane = view.pane_id();
    if pane == 0 {
        return;
    }
    let title = title.to_string();
    let body = body.to_string();
    let subtitle = view
        .window()
        .map(|w| w.title().to_string())
        .unwrap_or_default();

    let center = UNUserNotificationCenter::currentNotificationCenter();
    let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
    let block = RcBlock::new(
        move |granted: Bool, error: *mut objc2_foundation::NSError| {
            if !error.is_null() {
                eprintln!("ninja: notification authorization failed");
            }
            if granted.as_bool() {
                post(pane, &title, &body, &subtitle);
            }
        },
    );
    center.requestAuthorizationWithOptions_completionHandler(opts, &block);
    std::mem::forget(block);
}

/// 面成为 first responder：清掉这块面已投递的通知（Ghostty 同款）。
pub fn clear_delivered(pane: u32) {
    remove_ids(take_ids(pane));
}

/// 面拆除：清通知并丢掉跟踪。
pub fn forget_pane(pane: u32) {
    remove_ids(take_ids(pane));
}

/// 退出：清全部已投递通知。
pub fn shutdown() {
    UNUserNotificationCenter::currentNotificationCenter().removeAllDeliveredNotifications();
    if let Ok(mut map) = DELIVERED.lock() {
        map.clear();
    }
}

/// 主线程 tick 排干「点通知 → 聚焦」。`ghostty_app_tick` 之后调用。
pub fn drain_focus() {
    let pane = PENDING_FOCUS.swap(0, Ordering::AcqRel);
    if pane != 0 {
        focus_pane(pane);
    }
}

/// 前台且该面已聚焦 → 不弹 banner。纯逻辑，可单测。
pub(crate) fn should_present_banner(window_is_key: bool, surface_focused: bool) -> bool {
    !window_is_key || !surface_focused
}

pub(crate) fn parse_pane_id(s: &str) -> Option<u32> {
    s.parse().ok().filter(|&id| id != 0)
}

fn should_present_for_pane(pane: u32) -> bool {
    let Some(_mtm) = MainThreadMarker::new() else {
        return true;
    };
    let Some(view) = host::view_by_pane_id(pane) else {
        return false;
    };
    let Some(w) = view.window() else {
        return false;
    };
    should_present_banner(w.isKeyWindow(), surface_is_focused(&w, &view))
}

fn surface_is_focused(w: &NSWindow, view: &SurfaceHostView) -> bool {
    w.firstResponder().is_some_and(|r| {
        std::ptr::eq(
            r.as_ref() as *const NSResponder,
            as_responder(view) as *const NSResponder,
        )
    })
}

fn pane_from_notification(notification: &UNNotification) -> Option<u32> {
    let info = notification.request().content().userInfo();
    let key = NSString::from_str(USERINFO_PANE);
    let value: Option<Retained<NSString>> = unsafe { msg_send![&info, objectForKey: &*key] };
    value.and_then(|s| parse_pane_id(&s.to_string()))
}

fn post(pane: u32, title: &str, body: &str, subtitle: &str) {
    let id = format!(
        "ninja-pane-{pane}-{}",
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    if !body.is_empty() {
        content.setBody(&NSString::from_str(body));
    }
    if !subtitle.is_empty() {
        content.setSubtitle(&NSString::from_str(subtitle));
    }
    content.setSound(Some(&UNNotificationSound::defaultSound()));

    let pane_key = NSString::from_str(USERINFO_PANE);
    let pane_val = NSString::from_str(&pane.to_string());
    let info = NSDictionary::from_slices(&[&*pane_key], &[&*pane_val]);
    unsafe {
        let _: () = msg_send![&*content, setUserInfo: &*info];
    }

    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&id),
        &content,
        None,
    );
    remember_id(pane, id);

    let center = UNUserNotificationCenter::currentNotificationCenter();
    let block = RcBlock::new(move |error: *mut objc2_foundation::NSError| {
        if !error.is_null() {
            eprintln!("ninja: add notification failed");
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
    std::mem::forget(block);
}

fn remember_id(pane: u32, id: String) {
    if let Ok(mut map) = DELIVERED.lock() {
        map.entry(pane).or_default().push(id);
    }
}

fn take_ids(pane: u32) -> Vec<String> {
    DELIVERED
        .lock()
        .ok()
        .and_then(|mut map| map.remove(&pane))
        .unwrap_or_default()
}

fn forget_notification(notification: &UNNotification) {
    let id = notification.request().identifier().to_string();
    if let Ok(mut map) = DELIVERED.lock() {
        for ids in map.values_mut() {
            ids.retain(|s| s != &id);
        }
        map.retain(|_, ids| !ids.is_empty());
    }
}

fn remove_ids(ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let ns: Vec<Retained<NSString>> = ids.iter().map(|s| NSString::from_str(s)).collect();
    let arr = NSArray::from_retained_slice(&ns);
    UNUserNotificationCenter::currentNotificationCenter()
        .removeDeliveredNotificationsWithIdentifiers(&arr);
}

fn request_focus(pane: u32) {
    PENDING_FOCUS.store(pane, Ordering::Release);
    if let Some(rl) = objc2_core_foundation::CFRunLoop::main() {
        rl.wake_up();
    }
}

fn focus_pane(pane: u32) {
    let Some(view) = host::view_by_pane_id(pane) else {
        return;
    };
    let Some(w) = view.window() else {
        return;
    };
    if let Some(container) = crate::pane::container_of(&w)
        && container.is_zoomed()
        && container.zoomed_pane_id() != Some(pane)
    {
        container.unzoom();
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    {
        app.activateIgnoringOtherApps(true);
    }
    w.makeKeyAndOrderFront(None);
    let _ = w.makeFirstResponder(Some(as_responder(&view)));
}

#[cfg(test)]
mod tests {
    use super::{parse_pane_id, should_present_banner};

    #[test]
    fn banner_hidden_only_when_key_and_focused() {
        assert!(!should_present_banner(true, true));
        assert!(should_present_banner(true, false));
        assert!(should_present_banner(false, true));
        assert!(should_present_banner(false, false));
    }

    #[test]
    fn pane_id_rejects_zero_and_junk() {
        assert_eq!(parse_pane_id("2"), Some(2));
        assert_eq!(parse_pane_id("0"), None);
        assert_eq!(parse_pane_id(""), None);
        assert_eq!(parse_pane_id("nope"), None);
    }
}
