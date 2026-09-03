//! 粘贴板：Ghostty 同款「文件路径 / 文本」，外加剪贴板图片落临时文件。
//!
//! ⌘V 无字符串但有图时，把 PNG 写到 `$TMPDIR/ninja-clip-*.png`，把转义后的
//! 路径交给 libghostty 粘贴进 PTY。拖放只走 Ghostty 的 fileURL / string，
//! 不另存图片。

use std::sync::atomic::{AtomicU64, Ordering};

use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSPasteboard, NSPasteboardItem,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};

/// Ghostty `Shell.escape`：路径/URL 插进活终端时给 shell 敏感字符加反斜杠。
pub fn shell_escape(s: &str) -> String {
    const SPECIAL: &[char] = &[
        '\\', ' ', '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'', '`', '!', '#', '$', '&', ';',
        '|', '*', '?', '\t',
    ];
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if SPECIAL.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// ⌘V 要灌进 PTY 的文本：文件路径 / 字符串优先，否则剪贴板图片的临时路径。
pub fn paste_text(pb: &NSPasteboard) -> Option<String> {
    if let Some(s) = opinionated_text(pb) {
        return Some(s);
    }
    clipboard_image_path(pb).map(|p| shell_escape(&p))
}

/// Ghostty `getOpinionatedStringContents`：逐项 fileURL 路径（转义）或字符串。
pub fn opinionated_text(pb: &NSPasteboard) -> Option<String> {
    let items = pb.pasteboardItems()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(path) = file_url_path(&item) {
            parts.push(shell_escape(&path));
        } else if let Some(s) = item.stringForType(unsafe { NSPasteboardTypeString }) {
            parts.push(s.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// 拖放类型是否是 Ghostty 认的 string / fileURL。
pub fn has_drop_type(pb: &NSPasteboard) -> bool {
    let Some(types) = pb.types() else {
        return false;
    };
    types.iter().any(|t| {
        let string = unsafe { NSPasteboardTypeString };
        let file = unsafe { NSPasteboardTypeFileURL };
        &*t == string || &*t == file
    })
}

fn file_url_path(item: &NSPasteboardItem) -> Option<String> {
    if let Some(s) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) {
        return file_url_string_to_path(&s.to_string());
    }
    let plist = item.propertyListForType(unsafe { NSPasteboardTypeFileURL })?;
    if let Some(s) = downcast_nsstring(&plist) {
        return file_url_string_to_path(&s.to_string());
    }
    let url = downcast_nsurl(&plist)?;
    if !url.isFileURL() {
        return None;
    }
    url.path().map(|p| p.to_string())
}

fn file_url_string_to_path(s: &str) -> Option<String> {
    let url = NSURL::URLWithString(&NSString::from_str(s))?;
    if !url.isFileURL() {
        return None;
    }
    url.path().map(|p| p.to_string())
}

fn downcast_nsstring(obj: &AnyObject) -> Option<&NSString> {
    obj.downcast_ref::<NSString>()
}

fn downcast_nsurl(obj: &AnyObject) -> Option<&NSURL> {
    obj.downcast_ref::<NSURL>()
}

fn clipboard_image_path(pb: &NSPasteboard) -> Option<String> {
    let bytes = png_bytes(pb)?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ninja-clip-{}-{n}.png", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

fn png_bytes(pb: &NSPasteboard) -> Option<Vec<u8>> {
    if let Some(data) = pb.dataForType(unsafe { NSPasteboardTypePNG }) {
        let bytes = data.to_vec();
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    let tiff = pb
        .dataForType(unsafe { NSPasteboardTypeTIFF })
        .or_else(|| NSImage::initWithPasteboard(NSImage::alloc(), pb)?.TIFFRepresentation())?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
    let props: objc2::rc::Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, props.as_ref())
    }?;
    let bytes = png.to_vec();
    (!bytes.is_empty()).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_matches_ghostty() {
        let cases = [
            ("hello", "hello"),
            ("", ""),
            ("file name", "file\\ name"),
            ("a\\b", "a\\\\b"),
            ("(foo)", "\\(foo\\)"),
            ("[bar]", "\\[bar\\]"),
            ("{baz}", "\\{baz\\}"),
            ("<qux>", "\\<qux\\>"),
            ("say\"hi\"", "say\\\"hi\\\""),
            ("it's", "it\\'s"),
            ("`cmd`", "\\`cmd\\`"),
            ("wow!", "wow\\!"),
            ("#comment", "\\#comment"),
            ("$HOME", "\\$HOME"),
            ("a&b", "a\\&b"),
            ("a;b", "a\\;b"),
            ("a|b", "a\\|b"),
            ("*.txt", "\\*.txt"),
            ("file?.log", "file\\?.log"),
            ("col1\tcol2", "col1\\\tcol2"),
            ("$(echo 'hi')", "\\$\\(echo\\ \\'hi\\'\\)"),
            ("/tmp/my file (1).txt", "/tmp/my\\ file\\ \\(1\\).txt"),
        ];
        for (input, expected) in cases {
            assert_eq!(shell_escape(input), expected, "escape {input:?}");
        }
    }

    #[test]
    fn file_url_string_to_path_decodes() {
        assert_eq!(
            file_url_string_to_path("file:///Users/test/document.txt").as_deref(),
            Some("/Users/test/document.txt")
        );
        assert_eq!(
            file_url_string_to_path("file:///tmp/my%20file%20(1).txt").as_deref(),
            Some("/tmp/my file (1).txt")
        );
        assert_eq!(file_url_string_to_path("https://example.com/x"), None);
    }
}
