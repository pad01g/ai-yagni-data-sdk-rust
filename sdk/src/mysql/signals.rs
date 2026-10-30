//! SIGINT と SIGTERM を受けたら、このプロセスが立てたコンテナを止めてから終わる。
//!
//! シグナルのハンドラーでは旗を立てるだけにし（そこでできることは限られる）、見張りのスレッドが旗を見て片付ける。
//! libc の crate は使わず、std が既にリンクしている C の関数（`signal`・`kill`）を直接宣言する。
use std::sync::Mutex;
use std::sync::Once;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

/// このプロセスが立てて、まだ止めていないコンテナ。
static OWNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 受けたシグナル（0 なら受けていない）。
static RECEIVED: AtomicI32 = AtomicI32::new(0);
static INSTALL: Once = Once::new();

pub(super) fn own(container: &str) {
    OWNED.lock().unwrap_or_else(|e| e.into_inner()).push(container.to_owned());
}

pub(super) fn disown(container: &str) {
    OWNED.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| c != container);
}

#[cfg(unix)]
mod sys {
    use std::ffi::c_int;

    pub const SIGINT: c_int = 2;
    pub const SIGTERM: c_int = 15;
    const EPERM: i32 = 1;

    unsafe extern "C" {
        fn signal(signum: c_int, handler: usize) -> usize;
        fn kill(pid: c_int, sig: c_int) -> c_int;
    }

    extern "C" fn on_signal(sig: c_int) {
        super::RECEIVED.store(sig, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn install() {
        for sig in [SIGINT, SIGTERM] {
            // SAFETY: on_signal は原子的な書き込みしかしない（シグナルのハンドラーでしてよいこと）。
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
}

#[cfg(not(unix))]
mod sys {
    pub fn install() {}

    pub fn alive(_pid: i32) -> bool {
        true
    }
}

pub(super) fn alive(pid: i32) -> bool {
    pid > 0 && sys::alive(pid)
}

/// SIGINT と SIGTERM を受けたら、このプロセスが立てたコンテナを止めて終わるようにする（何度呼んでもよい）。
/// 終了コードは 128 ＋ シグナルの番号。
pub fn cleanup_on_signals() {
    INSTALL.call_once(|| {
        sys::install();
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_millis(100));
                let sig = RECEIVED.load(Ordering::SeqCst);
                if sig != 0 {
                    let owned = std::mem::take(&mut *OWNED.lock().unwrap_or_else(|e| e.into_inner()));
                    for c in owned {
                        let _ = super::docker(&["rm", "-f", &c], Duration::from_secs(60));
                    }
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
}
