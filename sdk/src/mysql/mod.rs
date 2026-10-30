//! 層0: 本物の MySQL を docker のコンテナとして立て、コンテナの中の mysql CLI で話す。
//!
//! - データは tmpfs に置く（使い捨て。速さのため）。ポートは公開しない。話す経路は `docker exec` だけ。
//! - 1 つのサーバーの上に、検査ごとに新しいデータベースを作って分ける（コンテナを立て直すより速い）。
//! - 環境変数 `AI_YAGNI_MYSQL` にコンテナ名があれば、それにつなぐ（立てて止めるのは呼んだ側の仕事）。
//! - スクリプト 1 つを mysql の 1 プロセス（＝1 本のコネクション）で流す。mysql はエラーに当たると終了し、
//!   その時点のトランザクションはサーバーが巻き戻す。終了をそのまま [`SqlError`] として返す。
//! - docker のコマンドは時間を区切る（既定 300 秒。`AI_YAGNI_MYSQL_TIMEOUT` か [`Server::timeout`] で変える）。
//!   時間を過ぎたら docker のクライアントを止めてエラーにする。
//! - 立てたコンテナには、持ち主（ホスト名とプロセス ID）と立てた時刻のラベルを付ける。新しく立てるときに、
//!   持ち主のプロセスがもういないもの（同じホストのものだけ分かる）と、古すぎるもの（既定 120 分。
//!   `AI_YAGNI_MYSQL_STALE_MINUTES`）を片付ける。`AI_YAGNI_MYSQL` のコンテナは片付けない。
mod signals;
mod sqltext;
mod xml;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use signals::cleanup_on_signals;
pub(crate) use sqltext::{first_word, single_statement};
pub use sqltext::{ident, literal, terminate};
pub use xml::{ResultSet, Row, parse_result_sets};

pub const IMAGE: &str = "mysql:8.4.6";
pub const ENV: &str = "AI_YAGNI_MYSQL";
/// docker のコマンド 1 回の時間の上限（秒）。
pub const TIMEOUT_ENV: &str = "AI_YAGNI_MYSQL_TIMEOUT";
/// これより古い（分）コンテナは、新しく立てるときに片付ける。
pub const STALE_ENV: &str = "AI_YAGNI_MYSQL_STALE_MINUTES";
/// ポートを公開しない使い捨てのサーバーのパスワード。
const PASSWORD: &str = "datafutures";
const LABEL: &str = "ai-yagni.sdk=mysql";
const OWNER_LABEL: &str = "ai-yagni.sdk.owner";
const STARTED_LABEL: &str = "ai-yagni.sdk.started";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_STALE_MINUTES: u64 = 120;

/// mysql が返したエラー。CLI 自体の失敗（docker、時間切れなど）なら code と sqlstate は None。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    pub message: String,
    pub code: Option<u32>,
    pub sqlstate: Option<String>,
}

impl SqlError {
    pub fn other(message: impl Into<String>) -> Self {
        Self { message: message.into(), code: None, sqlstate: None }
    }

    /// サーバーが返したエラーでなく、docker や CLI の失敗か（検査の結果としては数えない）。
    pub fn is_other(&self) -> bool {
        self.code.is_none()
    }

    /// 期待するエラーが、番号か SQLSTATE のどちらかで一致するか。
    pub fn is(&self, expected: &str) -> bool {
        self.code.is_some_and(|c| c.to_string() == expected) || self.sqlstate.as_deref() == Some(expected)
    }

    /// `ERROR 1062 (23000) at line 3: Duplicate entry ...` で始まる行を読む。
    pub fn from_stderr(stderr: &str) -> Self {
        let Some(line) = stderr.lines().find_map(|l| l.strip_prefix("ERROR ")) else {
            let s = stderr.trim();
            return Self::other(if s.is_empty() { "mysql exited" } else { s });
        };
        let (code, rest) = line.split_once(' ').unwrap_or((line, ""));
        let sqlstate = rest.strip_prefix('(').and_then(|r| r.get(..5)).map(str::to_owned);
        let message = rest.split_once(": ").map_or(rest, |(_, m)| m);
        match code.parse() {
            Ok(code) => Self { message: message.to_owned(), code: Some(code), sqlstate },
            Err(_) => Self::other(line),
        }
    }
}

impl std::fmt::Display for SqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.code, &self.sqlstate) {
            (Some(c), Some(s)) => write!(f, "{} ({c}, {s})", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for SqlError {}

/// 名前の重ならない 10 桁の 16 進（プロセス・時刻・呼んだ回数を混ぜる）。
fn random_hex() -> String {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u64(CALLS.fetch_add(1, Ordering::Relaxed));
    format!("{:010x}", h.finish() & 0xff_ffff_ffff)
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// コマンドの終わり方と出力。
struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn read_all(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// program を走らせ、input を標準入力に流す。timeout を過ぎたら止めて None を返す。
fn command(program: &str, args: &[&str], input: Option<String>, timeout: Duration) -> Result<Option<Output>, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    // 出力を読みながら書く（大きな結果でパイプが詰まらないように）
    let writer = input.zip(child.stdin.take()).map(|(input, mut stdin)| {
        thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        })
    });
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let until = Instant::now() + timeout;
    let mut pause = Duration::from_millis(1);
    let status = loop {
        match child.try_wait().map_err(|e| format!("{program}: {e}"))? {
            Some(status) => break Some(status),
            None if Instant::now() > until => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(20));
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    Ok(status.map(|s| Output { ok: s.success(), stdout, stderr }))
}

/// docker を走らせる。時間切れと、docker を走らせられないことはエラー。
fn docker_with(args: &[&str], input: Option<String>, timeout: Duration) -> Result<Output, SqlError> {
    let what = args.first().unwrap_or(&"");
    command("docker", args, input, timeout).map_err(SqlError::other)?.ok_or_else(|| {
        SqlError::other(format!(
            "docker {what} timed out after {}s (set {TIMEOUT_ENV} to wait longer)",
            timeout.as_secs()
        ))
    })
}

/// docker を走らせ、成功すれば標準出力を返す。
fn docker(args: &[&str], timeout: Duration) -> Result<String, SqlError> {
    let out = docker_with(args, None, timeout)?;
    if out.ok {
        Ok(out.stdout)
    } else {
        Err(SqlError::other(format!("docker {}: {}", args.first().unwrap_or(&""), out.stderr.trim())))
    }
}

/// このプロセスを表す持ち主のラベルの値（`ホスト名:プロセス ID`）。
fn owner() -> String {
    let host = command("hostname", &[], None, Duration::from_secs(5))
        .ok()
        .flatten()
        .map(|o| o.stdout.trim().to_owned())
        .unwrap_or_default();
    format!("{host}:{}", std::process::id())
}

/// 片付けてよいコンテナか。owner は `ホスト名:プロセス ID`、started は立てた時刻（UNIX 秒）。
/// 別のホスト（別のコンテナの中など）のプロセスは生きているか分からないので、古さだけで決める。
fn is_stale(owner: &str, started: &str, me: &str, now: u64, stale_after: u64) -> bool {
    let old = started.parse::<u64>().is_ok_and(|s| now.saturating_sub(s) > stale_after);
    let (host, pid) = owner.rsplit_once(':').unwrap_or(("", ""));
    let (my_host, _) = me.rsplit_once(':').unwrap_or(("", ""));
    let orphan = !host.is_empty() && host == my_host && pid.parse().is_ok_and(|p| !signals::alive(p));
    old || orphan
}

/// 前に立てて止め損ねたコンテナを片付ける（ほかの検査が使っているかもしれないものは残す）。
fn reap_stale(timeout: Duration) {
    let format = format!("{{{{.Names}}}}\t{{{{.Label \"{OWNER_LABEL}\"}}}}\t{{{{.Label \"{STARTED_LABEL}\"}}}}");
    let filter = format!("label={LABEL}");
    let Ok(list) = docker(&["ps", "-a", "--filter", &filter, "--format", &format], timeout) else { return };
    let shared = std::env::var(ENV).unwrap_or_default();
    let (me, now) = (owner(), now_secs());
    let stale_after = env_number(STALE_ENV).unwrap_or(DEFAULT_STALE_MINUTES) * 60;
    for line in list.lines() {
        let mut cols = line.split('\t');
        let (name, owner, started) = (cols.next().unwrap_or(""), cols.next().unwrap_or(""), cols.next().unwrap_or(""));
        if !name.is_empty() && name != shared && is_stale(owner, started, &me, now, stale_after) {
            let _ = docker(&["rm", "-f", name], timeout);
        }
    }
}

/// docker の中の MySQL サーバー。自分で立てたもの（owned）は drop で止める。
pub struct Server {
    container: String,
    owned: bool,
    timeout: Duration,
}

impl Server {
    fn new(container: String, owned: bool) -> Self {
        let timeout = env_number(TIMEOUT_ENV).map_or(DEFAULT_TIMEOUT, Duration::from_secs);
        Self { container, owned, timeout }
    }

    /// 新しいサーバーを立て、つながるまで待つ。前に止め損ねたコンテナを先に片付ける。
    pub fn start() -> Result<Self, SqlError> {
        let server = Self::new(format!("ai-yagni-mysql-{}", random_hex()), true);
        reap_stale(server.timeout);
        let pwd = format!("MYSQL_ROOT_PASSWORD={PASSWORD}");
        let owner = format!("{OWNER_LABEL}={}", owner());
        let started = format!("{STARTED_LABEL}={}", now_secs());
        signals::own(&server.container);
        docker(
            &[
                "run",
                "-d",
                "--rm",
                "--name",
                &server.container,
                "--label",
                LABEL,
                "--label",
                &owner,
                "--label",
                &started,
                "--tmpfs",
                "/var/lib/mysql",
                "-e",
                &pwd,
                IMAGE,
                "--skip-log-bin",
                "--innodb-flush-log-at-trx-commit=0",
                "--innodb-doublewrite=OFF",
                "--character-set-server=utf8mb4",
                "--collation-server=utf8mb4_0900_bin",
            ],
            server.timeout,
        )?;
        server.wait_ready(Duration::from_secs(180))?;
        Ok(server)
    }

    /// 既に立っているサーバー（コンテナ名）につなぐ。止めない。
    pub fn attach(container: impl Into<String>) -> Self {
        Self::new(container.into(), false)
    }

    /// `AI_YAGNI_MYSQL` のサーバーがあればそれに、なければ新しく立てる。
    pub fn obtain() -> Result<Self, SqlError> {
        match std::env::var(ENV) {
            Ok(name) if !name.is_empty() => Ok(Self::attach(name)),
            _ => Self::start(),
        }
    }

    /// docker のコマンド 1 回の時間の上限を変える。
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn container(&self) -> &str {
        &self.container
    }

    /// コンテナの中で mysql を走らせる `docker exec` の引数。
    fn mysql_args<'a>(&'a self, pwd: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["exec", "-i", "-e", pwd, &self.container, "mysql", "-uroot", "-h127.0.0.1"];
        args.extend(extra);
        args
    }

    fn wait_ready(&self, timeout: Duration) -> Result<(), SqlError> {
        // 初期化中の一時サーバーは TCP を開かないので、127.0.0.1 で SELECT が通れば本番のサーバーが上がっている
        let until = Instant::now() + timeout;
        let pwd = format!("MYSQL_PWD={PASSWORD}");
        loop {
            match docker(&self.mysql_args(&pwd, &["-e", "SELECT 1"]), self.timeout) {
                Ok(_) => return Ok(()),
                Err(e) if Instant::now() > until => {
                    return Err(SqlError::other(format!("mysql did not become ready: {e}")));
                }
                Err(_) => thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    /// スクリプトを 1 本のコネクションで流し、結果を返す文の結果を順に返す。
    pub fn run(&self, database: Option<&str>, script: &str) -> Result<Vec<ResultSet>, SqlError> {
        let pwd = format!("MYSQL_PWD={PASSWORD}");
        let mut extra = vec!["--xml", "--default-character-set=utf8mb4"];
        if let Some(db) = database {
            extra.extend(["-D", db]);
        }
        let input = format!("SET time_zone = '+00:00';\n{}", terminate(script));
        let out = docker_with(&self.mysql_args(&pwd, &extra), Some(input), self.timeout)?;
        if !out.ok {
            return Err(SqlError::from_stderr(&out.stderr));
        }
        Ok(parse_result_sets(&out.stdout))
    }

    /// 新しい空のデータベースを作って名前を返す。
    pub fn create_database(&self, prefix: &str) -> Result<String, SqlError> {
        let clean: String =
            prefix.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        let clean: String = clean.chars().take(40).collect();
        let name = format!("{clean}_{}", random_hex());
        self.run(None, &format!("CREATE DATABASE {}", ident(&name).map_err(SqlError::other)?))?;
        Ok(name)
    }

    pub fn drop_database(&self, name: &str) -> Result<(), SqlError> {
        self.run(None, &format!("DROP DATABASE IF EXISTS {}", ident(name).map_err(SqlError::other)?))?;
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.owned {
            let _ = docker(&["rm", "-f", &self.container], self.timeout);
            signals::disown(&self.container);
        }
    }
}

/// MySQL を 1 つ立て、`AI_YAGNI_MYSQL` にそのコンテナ名を入れて args のコマンドを走らせ、終わったら止める。
/// 終了コードを返す。テスト全体で 1 つのサーバーを共有するために、利用者の小さな bin から呼ぶ（`with-mysql cargo test`）。
/// SIGINT か SIGTERM を受けたときも、立てたコンテナを止めてから終わる。
pub fn with_server(args: &[String]) -> i32 {
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("usage: with-mysql <command> [args...]");
        return 2;
    };
    cleanup_on_signals();
    let server = match Server::start() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot start mysql: {e}");
            return 2;
        }
    };
    let status = Command::new(cmd).args(rest).env(ENV, server.container()).status();
    drop(server);
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("{cmd}: {e}");
            2
        }
    }
}

/// サーバーの上の 1 つのデータベース。言明はここで確かめる。
pub struct Db<'s> {
    pub server: &'s Server,
    pub database: String,
}

impl Db<'_> {
    pub fn run(&self, script: &str) -> Result<Vec<ResultSet>, SqlError> {
        self.server.run(Some(&self.database), script)
    }

    /// 結果を返す文がちょうど 1 つのスクリプトの、その結果の行。結果が 1 つでなければエラー。
    pub fn query(&self, script: &str) -> Result<Vec<Row>, SqlError> {
        let mut sets = self.run(script)?;
        match sets.len() {
            1 => Ok(sets.remove(0).rows),
            n => Err(SqlError::other(format!("expected exactly one result, got {n}"))),
        }
    }
}

/// 新しく作り、drop で消すデータベース。
pub struct Scratch<'s> {
    pub db: Db<'s>,
}

impl<'s> Scratch<'s> {
    pub fn create(server: &'s Server, label: &str) -> Result<Self, SqlError> {
        let database = server.create_database(label)?;
        Ok(Self { db: Db { server, database } })
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        let _ = self.db.server.drop_database(&self.db.database);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mysql_errors() {
        let e = SqlError::from_stderr("ERROR 1062 (23000) at line 3: Duplicate entry 'x' for key 'PRIMARY'\n");
        assert_eq!(e.code, Some(1062));
        assert_eq!(e.sqlstate.as_deref(), Some("23000"));
        assert_eq!(e.message, "Duplicate entry 'x' for key 'PRIMARY'");
        assert!(e.is("1062") && e.is("23000") && !e.is("45000") && !e.is_other());
        let s = SqlError::from_stderr("ERROR 1644 (45000): transfers are append-only");
        assert_eq!(s.message, "transfers are append-only");
        assert_eq!(SqlError::from_stderr("").message, "mysql exited");
        assert!(SqlError::from_stderr("").is_other());
    }

    #[test]
    fn only_a_line_that_starts_with_error_is_read_as_the_error() {
        let e = SqlError::from_stderr(
            "mysql: [Warning] see ERROR 9 here\nERROR 1146 (42S02) at line 1: Table 'x' doesn't exist\n",
        );
        assert_eq!(e.code, Some(1146));
        assert!(SqlError::from_stderr("docker: Error response: no ERROR 1 here").is_other());
    }

    #[test]
    fn random_names_differ() {
        assert_ne!(random_hex(), random_hex());
        assert_eq!(random_hex().len(), 10);
    }

    #[test]
    fn stale_containers_are_old_or_orphaned_on_this_host() {
        let (me, now) = ("box:100", 10_000);
        // 若く、持ち主が別のホスト: 生きているか分からないので残す
        assert!(!is_stale("other:1", "9990", me, now, 600));
        // 古い
        assert!(is_stale("other:1", "1000", me, now, 600));
        // 同じホストで、持ち主のプロセスがもういない
        assert!(is_stale("box:2147483646", "9990", me, now, 600));
        // 同じホストで、持ち主が生きている（このプロセス）
        assert!(!is_stale(&format!("box:{}", std::process::id()), "9990", me, now, 600));
        // ラベルのない古い版のコンテナは、いつ誰が立てたか分からないので残す
        assert!(!is_stale("", "", me, now, 600));
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped() {
        let started = Instant::now();
        assert!(command("sleep", &["30"], None, Duration::from_millis(200)).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        let out = command("cat", &[], Some("hello".to_owned()), Duration::from_secs(10)).unwrap().unwrap();
        assert!(out.ok && out.stdout == "hello");
    }
}
