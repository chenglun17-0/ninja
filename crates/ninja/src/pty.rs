//! PTY：forkpty 拉起 `$SHELL`（缺省 bash），读写各一条线程。
//!
//! 数据流（libghostty-vt 非线程安全，所有 vt 调用只在主线程）：
//!
//! ```text
//! shell ──slave── master fd ──读线程──> rx 队列 ──CFRunLoopSource──> 主线程
//!                                                                     │
//! 键盘/粘贴 ──> 主线程 ──写线程（condvar）──> master fd ──slave──> shell
//! ```
//!
//! resize：主线程 `ioctl(TIOCSWINSZ)`，内核给 shell 的前台进程组发 SIGWINCH，
//! shell 重画，走回上面的读路径——宿主自己不碰信号。
//!
//! 读线程只搬运字节（不碰 vt / GUI 对象）；写线程阻塞 `write(2)`，
//! 大段粘贴塞满管道时由它背压，不卡主线程。

use std::collections::VecDeque;
use std::ffi::CString;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// 从 PTY 读到、待主线程消费的事件。
#[derive(Debug)]
pub enum PtyEvent {
    /// master fd 上读到的原始 VT 字节。
    Bytes(Vec<u8>),
    /// 读到 EOF：shell 退出。
    Eof,
}

/// 主线程与读写线程共享的 IO 核。
pub struct PtyInner {
    master: AtomicI32,
    child: AtomicI32,
    /// 写线程工作队列。
    to_pty: Mutex<VecDeque<Vec<u8>>>,
    to_pty_wake: Condvar,
    /// 主线程消费队列（读线程 push，主线程 drain）。
    rx: Mutex<VecDeque<PtyEvent>>,
    /// 主线程注册的唤醒钩子（信号该 pane 的 CFRunLoopSource 并唤醒
    /// 主 runloop）。p2：每个 PTY 一个钩子，多 pane 各自唤醒，不再全局单例。
    /// 钩子只能在 PTY 读写线程被调；主线程在 drop PTY（join 读写线程）
    /// 之后才拆 source，无并发窗口。
    wake: Mutex<Option<WakeFn>>,
}

/// 主线程唤醒钩子。闭包在 PTY 读线程上执行，只做线程安全动作
/// （CFRunLoopSourceSignal + CFRunLoopWakeUp），不碰 GUI/vt。
pub type WakeFn = std::sync::Arc<dyn Fn() + Send + Sync>;

impl PtyInner {
    /// 往 PTY 写（任意线程调用；实际 write 在写线程）。
    pub fn write(&self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let mut q = self.to_pty.lock().unwrap();
        q.push_back(data);
        self.to_pty_wake.notify_one();
    }

    /// 主线程 drain 读到的全部事件。返回是否含 Eof。
    pub fn drain(&self) -> (Vec<Vec<u8>>, bool) {
        let mut q = self.rx.lock().unwrap();
        let mut chunks = Vec::new();
        let mut eof = false;
        while let Some(ev) = q.pop_front() {
            match ev {
                PtyEvent::Bytes(b) => chunks.push(b),
                PtyEvent::Eof => eof = true,
            }
        }
        (chunks, eof)
    }

    /// rx 队列里是否还有事件（非阻塞 peek）。D-C 洪峰合帧用：读线程
    /// 在本次 perform 期间又压入了字节就先不重画——下一个 perform
    /// 会带上最新状态再画（push 先于 signal，看到 push 必有后续
    /// fire，不会丢帧）。
    pub fn has_pending(&self) -> bool {
        match self.rx.try_lock() {
            Ok(q) => !q.is_empty(),
            Err(_) => true, // 主线程正持有（不可能：drain 已还锁）→保守画
        }
    }

    /// 子进程 pid（用于关窗时 SIGHUP 整个进程组）。
    pub fn child_pid(&self) -> libc::pid_t {
        self.child.load(Ordering::Acquire)
    }

    /// 主线程注册/注销唤醒钩子（每 pane 一次，见 view 的 install_wake）。
    /// 注销后（None）读线程不再有唤醒路径——视图侧在 drop PTY 前先注销。
    pub fn set_wake(&self, wake: Option<WakeFn>) {
        *self.wake.lock().unwrap() = wake;
    }

    fn wake_main(&self) {
        let f = self.wake.lock().unwrap().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// 收尾：给 shell 进程组发 SIGHUP、关 master。幂等。
    pub fn shutdown(&self) {
        let pid = self.child.swap(0, Ordering::AcqRel);
        if pid > 0 {
            // forkpty 后子进程是会话组长/进程组长；杀组能把 shell 的
            // 子进程（vim、cat …）一起带走。
            unsafe {
                libc::kill(-pid, libc::SIGHUP);
                libc::kill(pid, libc::SIGHUP);
                // 不 waitpid 阻塞主线程；进程退出时由 init 收尸。
            }
        }
        let fd = self.master.swap(-1, Ordering::AcqRel);
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }
}

/// 一个 PTY 会话。Drop 时 shutdown（SIGHUP + close master）。
pub struct Pty {
    pub inner: Arc<PtyInner>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Pty {
    /// forkpty 拉起 shell。`command` 为空时用 `$SHELL`，再缺省 `/bin/bash`
    ///（plan：拉起 $SHELL 默认 bash）。argv[0] 加 `-` 前缀 → 登录 shell 语义。
    pub fn spawn(command: Option<&str>, cols: u16, rows: u16) -> std::io::Result<Self> {
        let shell = match command {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
        };

        let mut ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let mut master: libc::c_int = -1;
        // SAFETY: master/ws out-param 布局正确；fork 后父子各自走安全路径。
        let pid = unsafe {
            libc::forkpty(
                &raw mut master,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut ws,
            )
        };
        if pid < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if pid == 0 {
            // 子进程：forkpty 已 setsid+login_tty。此后只做 async-signal-safe 调用，
            // 直到 execvp。
            unsafe {
                libc::setsid();
                // 让 shell 知道自己是什么终端。
                libc::setenv(
                    b"TERM\0".as_ptr().cast(),
                    b"xterm-256color\0".as_ptr().cast(),
                    1,
                );
                libc::setenv(
                    b"COLORTERM\0".as_ptr().cast(),
                    b"truecolor\0".as_ptr().cast(),
                    1,
                );
                libc::unsetenv(b"TMUX\0".as_ptr().cast());
                // X4：GUI 进程（Dock/Finder/open 启动）的 cwd 是 /，不 chdir
                // 的话新 shell 打开就落在根目录。终端惯例：新 pane 默认
                // 在家目录；HOME 取不到时保持现状 cwd。
                let home_ptr = libc::getenv(b"HOME\0".as_ptr().cast());
                if !home_ptr.is_null() {
                    libc::chdir(home_ptr);
                }
                // argv[0] 带 `-` 前缀 = 登录 shell（iTerm/Terminal.app 同款做法）。
                let argv0 =
                    CString::new(format!("-{}", shell.rsplit('/').next().unwrap_or(&shell)))
                        .unwrap_or_else(|_| CString::new("-bash").unwrap());
                let shell_c = CString::new(shell.as_str()).unwrap();
                libc::execv(
                    shell_c.as_ptr(),
                    [argv0.as_ptr(), std::ptr::null()].as_mut_ptr(),
                );
                // exec 失败：唯一安全的退出路径。
                libc::_exit(127);
            }
        }

        // 父进程：master 非阻塞（读线程 poll 心跳 + 写线程自己管理阻塞语义）。
        unsafe {
            let flags = libc::fcntl(master, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        let inner = Arc::new(PtyInner {
            master: AtomicI32::new(master),
            child: AtomicI32::new(pid),
            to_pty: Mutex::new(VecDeque::new()),
            to_pty_wake: Condvar::new(),
            rx: Mutex::new(VecDeque::new()),
            wake: Mutex::new(None),
        });

        // 读线程：阻塞在 poll 上，有数据就 read，EOF 汇报后退出。
        let r_inner = Arc::clone(&inner);
        let reader = std::thread::Builder::new()
            .name("ninja-pty-read".into())
            .spawn(move || reader_loop(r_inner, master))
            .map_err(std::io::Error::other)?;

        // 写线程：condvar 驱动的 write_all。
        let w_inner = Arc::clone(&inner);
        let writer = std::thread::Builder::new()
            .name("ninja-pty-write".into())
            .spawn(move || writer_loop(w_inner, master))
            .map_err(std::io::Error::other)?;

        Ok(Self {
            inner,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    /// resize：改内核 winsize → SIGWINCH → shell。
    pub fn resize(&self, cols: u16, rows: u16, cell_w_px: u32, cell_h_px: u32) {
        let fd = self.inner.master.load(Ordering::Acquire);
        if fd < 0 {
            return;
        }
        let mut ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: (u32::from(cols) * cell_w_px) as u16,
            ws_ypixel: (u32::from(rows) * cell_h_px) as u16,
        };
        // SAFETY: ws 布局正确；fd 属于我们。
        unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &raw mut ws) };
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.inner.shutdown();
        // 唤醒写线程让它退出。
        {
            let _guard = self.inner.to_pty.lock().unwrap();
            self.inner.to_pty_wake.notify_all();
        }
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
        if let Some(t) = self.writer.take() {
            let _ = t.join();
        }
    }
}

/// 读线程主体。master 已非阻塞：poll 等可读，read 到 EAGAIN 继续等，
/// 0 = EOF，负值且非 EINTR/EAGAIN = 出错（当作 Eof 处理，让主线程收尾）。
fn reader_loop(inner: Arc<PtyInner>, fd: libc::c_int) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if inner.master.load(Ordering::Acquire) < 0 {
            break; // 已 shutdown
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd 布局正确。
        let r = unsafe { libc::poll(&raw mut pfd, 1, 500) };
        if r < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            push_event(&inner, PtyEvent::Eof);
            break;
        }
        if r == 0 || pfd.revents & libc::POLLIN == 0 {
            continue;
        }
        // SAFETY: buf 长度有效；fd 属于我们。
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            let data = buf[..n as usize].to_vec();
            push_event(&inner, PtyEvent::Bytes(data));
        } else if n == 0 {
            push_event(&inner, PtyEvent::Eof);
            break;
        } else {
            let err = std::io::Error::last_os_error();
            match err.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => continue,
                // EBADF 等：master 已关，按 EOF 收尾。
                _ => {
                    push_event(&inner, PtyEvent::Eof);
                    break;
                }
            }
        }
    }
}

/// 写线程主体：等队列 → write_all → 阻塞在管道上天然背压。
fn writer_loop(inner: Arc<PtyInner>, fd: libc::c_int) {
    loop {
        let mut q = inner.to_pty.lock().unwrap();
        while q.is_empty() {
            // 主线程 shutdown 时 to_pty 不再投递，靠 master 关闭后 write 失败退出；
            // shutdown() 也会 notify_all 这里。
            q = match inner
                .to_pty_wake
                .wait_timeout(q, std::time::Duration::from_secs(1))
            {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
            if inner.master.load(Ordering::Acquire) < 0 {
                return;
            }
        }
        let chunk = q.pop_front().unwrap();
        drop(q);

        let mut off = 0usize;
        while off < chunk.len() {
            if inner.master.load(Ordering::Acquire) < 0 {
                return;
            }
            // SAFETY: chunk 切片有效；fd 属于我们。
            let n = unsafe { libc::write(fd, chunk[off..].as_ptr().cast(), chunk.len() - off) };
            if n >= 0 {
                off += n as usize;
            } else {
                let err = std::io::Error::last_os_error();
                match err.kind() {
                    std::io::ErrorKind::WouldBlock => {
                        // 非阻塞 master 写满：poll 等可写，别忙转。
                        let mut pfd = libc::pollfd {
                            fd,
                            events: libc::POLLOUT,
                            revents: 0,
                        };
                        // SAFETY: pfd 布局正确。
                        unsafe { libc::poll(&raw mut pfd, 1, -1) };
                    }
                    std::io::ErrorKind::Interrupted => continue,
                    _ => return, // EPIPE/EBADF：对端没了
                }
            }
        }
    }
}

fn push_event(inner: &Arc<PtyInner>, ev: PtyEvent) {
    inner.rx.lock().unwrap().push_back(ev);
    // 唤醒主线程：信号该 pane 的 CFRunLoopSource 并唤醒主 runloop
    //（钩子由 view 侧按 pane 注册）。
    inner.wake_main();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_cat_echo_roundtrip() {
        // /bin/cat：写给它的东西原样回来，证明 master/slave 双向都通。
        let pty = Pty::spawn(Some("/bin/cat"), 40, 10).unwrap();
        pty.inner.write(b"hello-ninja\n".to_vec());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = Vec::new();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let (chunks, _) = pty.inner.drain();
            for c in chunks {
                got.extend_from_slice(&c);
            }
            if got
                .windows(b"hello-ninja\n".len())
                .any(|w| w == b"hello-ninja\n")
            {
                break;
            }
        }
        assert!(
            got.windows(11).any(|w| w == b"hello-ninja"),
            "echo not seen, got {:?}",
            String::from_utf8_lossy(&got)
        );

        // resize ioctl 不报错（shell 侧效果不在此断言）。
        pty.resize(100, 30, 10, 20);
    }

    /// X4 回归：新 shell 的 cwd 必须是 $HOME（GUI 进程 cwd=/ 的坑）。
    #[test]
    fn spawn_shell_starts_in_home() {
        let home = std::env::var("HOME").expect("测试环境必须有 HOME");
        let pty = Pty::spawn(Some("/bin/sh"), 40, 10).unwrap();
        pty.inner.write(b"pwd\nexit\n".to_vec());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut got = Vec::new();
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let (chunks, _) = pty.inner.drain();
            for chunk in chunks {
                got.extend_from_slice(&chunk);
            }
            if let Ok(text) = std::str::from_utf8(&got) {
                if text.contains(&home) {
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&got);
        assert!(
            text.contains(&home),
            "pwd 应输出家目录 {home}，实际：{text}"
        );
    }

    /// D-C 回归：has_pending peek——drain 后为 false，写过去回声到达后
    /// 为 true（洪峰合帧的判据；push 先于 signal，看到 true 必有后续 fire）。
    #[test]
    fn has_pending_tracks_rx_queue() {
        let pty = Pty::spawn(Some("/bin/cat"), 40, 10).unwrap();
        // 静默期：队列空。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pty.inner.has_pending() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let _ = pty.inner.drain();
        }
        assert!(!pty.inner.has_pending(), "静默期队列应空");

        // 写入 → cat 回声 → 读线程压进 rx → has_pending 为真。
        pty.inner.write(b"peek\n".to_vec());
        let mut got = false;
        while std::time::Instant::now() < deadline {
            if pty.inner.has_pending() {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(got, "回声入队后 has_pending 应为真");

        // drain 干净 → 回 false。
        let _ = pty.inner.drain();
        assert!(!pty.inner.has_pending());
    }

    #[test]
    fn shutdown_is_idempotent() {
        let pty = Pty::spawn(Some("/bin/cat"), 20, 5).unwrap();
        pty.inner.shutdown();
        pty.inner.shutdown();
    }
}
