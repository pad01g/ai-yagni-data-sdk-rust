//! SIGINT と SIGTERM を受けたら、このプロセスが立てたコンテナと作ったデータベースを片付けてから終わる。
//!
//! シグナルのハンドラーでは旗を立て、ハンドラーを既定（SIG_DFL）に戻すだけにする（そこでできることは限られる）。
//! 2 度目のシグナルは既定の動き（すぐに終わる）になる。見張りのスレッドが旗を見て、次の順に片付ける。
//!
//! 1. 子のプロセス（`with-mysql` が走らせたコマンド）にシグナルを渡し、終わるのを待つ（長くても 60 秒）。
//! 2. 途中の `docker run` が終わるのを待つ（長くても 120 秒。終わらないうちに消すと、後から立ったコンテナが残る）。
//! 3. 既にあるサーバーの上に作ったデータベースを、このプロセスのコネクションを止めてから消す。
//! 4. 立てたコンテナを消す。
//!
//! libc の crate は使わず、std が既にリンクしている C の関数（`signal`・`kill`）を直接宣言する。
use std::sync::Mutex;
use std::sync::Once;
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// このプロセスが立てて、まだ止めていないコンテナ。
static OWNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 終わるときに流す片付け（番号, コンテナ, SQL）。
static CLEANUP: Mutex<Vec<(u64, String, String)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
/// 走らせている子のプロセス。
static CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// 途中の `docker run` の数。
static STARTING: AtomicUsize = AtomicUsize::new(0);
/// 受けたシグナル（0 なら受けていない）。
static RECEIVED: AtomicI32 = AtomicI32::new(0);
static INSTALL: Once = Once::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn own(container: &str) {
    lock(&OWNED).push(container.to_owned());
}

pub(super) fn disown(container: &str) {
    lock(&OWNED).retain(|c| c != container);
}

/// 終わるときに container で sql を流すよう覚える。返した番号を [`done`] に渡すと忘れる。
pub(super) fn on_exit(container: &str, sql: &str) -> u64 {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    lock(&CLEANUP).push((id, container.to_owned(), sql.to_owned()));
    id
}

pub(super) fn done(id: u64) {
    lock(&CLEANUP).retain(|(i, _, _)| *i != id);
}

pub(super) fn child(pid: u32) {
    lock(&CHILDREN).push(pid);
}

pub(super) fn child_done(pid: u32) {
    lock(&CHILDREN).retain(|p| *p != pid);
}

/// 受けたシグナル（受けていなければ 0）。
pub(super) fn received() -> i32 {
    RECEIVED.load(Ordering::SeqCst)
}

/// 生きている間、途中の `docker run` として数える。
pub(super) struct Starting;

impl Starting {
    pub(super) fn new() -> Self {
        STARTING.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Starting {
    fn drop(&mut self) {
        STARTING.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(unix)]
mod sys {
    use std::ffi::c_int;

    pub const SIGINT: c_int = 2;
    pub const SIGTERM: c_int = 15;
    const SIG_DFL: usize = 0;
    const EPERM: i32 = 1;

    unsafe extern "C" {
        fn signal(signum: c_int, handler: usize) -> usize;
        fn kill(pid: c_int, sig: c_int) -> c_int;
    }

    extern "C" fn on_signal(sig: c_int) {
        super::RECEIVED.store(sig, std::sync::atomic::Ordering::SeqCst);
        // 2 度目は既定の動き（すぐに終わる）にする。signal はハンドラーの中で呼んでよい。
        // SAFETY: SIG_DFL を設定するだけ。
        unsafe {
            signal(SIGINT, SIG_DFL);
            signal(SIGTERM, SIG_DFL);
        }
    }

    pub fn install() {
        for sig in [SIGINT, SIGTERM] {
            // SAFETY: on_signal は原子的な書き込みと signal しかしない（シグナルのハンドラーでしてよいこと）。
            unsafe {
                signal(sig, on_signal as extern "C" fn(c_int) as usize);
            }
        }
    }

    /// プロセスが生きているか（シグナル 0 を送って確かめる。権限がなくても、いることは分かる）。
    pub fn alive(pid: i32) -> bool {
        // SAFETY: シグナル 0 は何も届けず、相手がいるかだけを確かめる。
        let r = unsafe { kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(EPERM)
    }

    pub fn send(pid: i32, sig: i32) {
        // SAFETY: 自分が走らせた子のプロセスに、受けたシグナルを渡すだけ。
        unsafe {
            kill(pid, sig);
        }
    }
}

#[cfg(not(unix))]
mod sys {
    pub fn install() {}

    pub fn alive(_pid: i32) -> bool {
        true
    }

    pub fn send(_pid: i32, _sig: i32) {}
}

pub(super) fn alive(pid: i32) -> bool {
    pid > 0 && sys::alive(pid)
}

/// cond が成り立つまで、長くても limit だけ待つ。
fn wait_until(limit: Duration, cond: impl Fn() -> bool) {
    let until = Instant::now() + limit;
    while !cond() && Instant::now() < until {
        thread::sleep(Duration::from_millis(100));
    }
}

/// シグナルを受けた後の片付け。
fn clean_up(sig: i32) {
    let children = lock(&CHILDREN).clone();
    for pid in &children {
        sys::send(*pid as i32, sig);
    }
    if !children.is_empty() {
        wait_until(Duration::from_secs(60), || lock(&CHILDREN).is_empty());
    }
    wait_until(Duration::from_secs(120), || STARTING.load(Ordering::SeqCst) == 0);
    let owned = std::mem::take(&mut *lock(&OWNED));
    let cleanup = std::mem::take(&mut *lock(&CLEANUP));
    let mut killed: Vec<&str> = Vec::new();
    for (_, container, sql) in &cleanup {
        if owned.contains(container) {
            continue;
        }
        if !killed.contains(&container.as_str()) {
            super::kill_marked(container, super::OWNER_MARK, super::process_mark());
            killed.push(container);
        }
        let _ = super::Server::attach(container.as_str()).timeout(super::SHORT).run(None, sql);
    }
    for c in owned {
        let _ = super::docker(&["rm", "-f", &c], Duration::from_secs(60));
    }
}

/// SIGINT と SIGTERM を受けたら、このプロセスが立てたコンテナと作ったデータベースを片付けて終わるようにする
/// （何度呼んでもよい）。終了コードは 128 ＋ シグナルの番号。
pub fn cleanup_on_signals() {
    INSTALL.call_once(|| {
        sys::install();
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_millis(100));
                let sig = received();
                if sig != 0 {
                    clean_up(sig);
                    std::process::exit(128 + sig);
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id() as i32));
        assert!(!alive(0));
    }

    #[test]
    fn cleanups_are_remembered_until_done() {
        let id = on_exit("c", "DROP DATABASE IF EXISTS `x`");
        assert!(lock(&CLEANUP).iter().any(|(i, _, _)| *i == id));
        done(id);
        assert!(!lock(&CLEANUP).iter().any(|(i, _, _)| *i == id));
    }
}
