//! 層0: 本物の MySQL を docker のコンテナとして立て、コンテナの中の mysql CLI で話す。
//!
//! - データは tmpfs に置く（使い捨て。速さのため）。ポートは公開しない。話す経路は `docker exec` だけ。
//! - 1 つのサーバーの上に、検査ごとに新しいデータベースを作って分ける（コンテナを立て直すより速い）。
//!   分けるのはデータベースだけで、サーバーの設定や利用者は分けない（そうした文は検査の規則の節で止める）。
//! - 環境変数 `AI_YAGNI_MYSQL` にコンテナ名があれば、それにつなぐ（立てて止めるのは呼んだ側の仕事）。
//! - スクリプト 1 つを mysql の 1 プロセス（＝1 本のコネクション）で流す。mysql はエラーに当たると終了し、
//!   その時点のトランザクションはサーバーが巻き戻す。終了をそのまま [`SqlError`] として返す。
//! - mysql CLI は `--binary-mode` で走らせる（`\g` や `\!` などの CLI のコマンドを拒む）。時刻帯は
//!   `--init-command` で UTC にする（スクリプトの前に文を足さないので、エラーの行番号はファイルの行番号のまま）。
//! - [`Scratch::build`] は、データベースを作ってスクリプトを順に流すところまでを 1 回の `docker exec` で行う。
//!   スクリプトは 1 本ずつ別の mysql のプロセス（別のコネクション）で流すので、セッションの状態は漏れない。
//!   これにはコンテナの中に `sh`・`sed`・`base64`・`mktemp` が要る（公式の mysql のイメージにはある）。
//! - docker のコマンドは時間を区切る（既定 300 秒。`AI_YAGNI_MYSQL_TIMEOUT` か [`Server::timeout`] で変える）。
//!   時間を過ぎたら、サーバーの上のそのコネクションを `KILL` し、docker のクライアントを止めてエラーにする。
//!   コネクションは `--init-command` で付けた印（ユーザー変数）を performance_schema で探して見つける。
//! - 立てたコンテナには、持ち主（ホスト名・プロセス ID・PID 名前空間・起動の ID）と立てた時刻のラベルを付ける。
//!   新しく立てるときに、次のものだけを片付ける（迷ったら残す。消し損ねは後で片付くが、使っているものは戻らない）。
//!   - 持ち主が同じカーネルの同じ PID 名前空間にいて、そのプロセスがもういないもの。
//!   - 持ち主が生きているか確かめられず、立ててから `AI_YAGNI_MYSQL_STALE_MINUTES`（既定 720 分）より古いもの。
//!   - ただし、このプロセスか、見えるほかのプロセスの `AI_YAGNI_MYSQL` が指すコンテナは片付けない。
mod signals;
mod sqltext;
mod xml;

use std::any::{Any, TypeId};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use signals::cleanup_on_signals;
pub(crate) use sqltext::{first_word, rewrite_grants, server_side, server_wide, single_statement, statements};
pub use sqltext::{ident, literal, terminate};
pub use xml::{ResultSet, Row, parse_result_sets};

pub const IMAGE: &str = "mysql:8.4.6";
pub const ENV: &str = "AI_YAGNI_MYSQL";
/// docker のコマンド 1 回の時間の上限（秒）。
pub const TIMEOUT_ENV: &str = "AI_YAGNI_MYSQL_TIMEOUT";
/// 持ち主が生きているか確かめられないコンテナは、これより古い（分）ときだけ、新しく立てるときに片付ける。
pub const STALE_ENV: &str = "AI_YAGNI_MYSQL_STALE_MINUTES";
/// ポートを公開しない使い捨てのサーバーのパスワード。
const PASSWORD: &str = "datafutures";
const LABEL: &str = "ai-yagni.sdk=mysql";
const OWNER_LABEL: &str = "ai-yagni.sdk.owner";
const STARTED_LABEL: &str = "ai-yagni.sdk.started";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_STALE_MINUTES: u64 = 720;
/// 消すのを待つデータベースがこれだけたまったら、まとめて消す。
const DOOMED_BATCH: usize = 16;
/// 片付けのための短い時間の上限。
const SHORT: Duration = Duration::from_secs(30);
/// コネクションに付ける印（ユーザー変数の名前）。1 回の呼び出しごとの印と、プロセスごとの印。
const RUN_MARK: &str = "ai_yagni_run";
const OWNER_MARK: &str = "ai_yagni_owner";

/// mysql が返したエラー。CLI 自体の失敗（docker、時間切れなど）なら code と sqlstate は None。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    pub message: String,
    pub code: Option<u32>,
    pub sqlstate: Option<String>,
    /// エラーが起きた行（流したスクリプトの 1 行目から数える）。
    pub line: Option<u32>,
}

impl SqlError {
    pub fn other(message: impl Into<String>) -> Self {
        Self { message: message.into(), code: None, sqlstate: None, line: None }
    }

    /// サーバーが SQL に下した判定でなく、docker・CLI・接続の失敗か（検査の結果としては数えない）。
    ///
    /// 番号のないもの、クライアントのエラー（2000〜2999。2013 Lost connection、2006 gone away など）、
    /// サーバーの接続と停止のエラー（1040 接続が多すぎる、1053 停止中、1152〜1161 通信、1317 中断、3169 セッションを切られた）。
    pub fn is_other(&self) -> bool {
        match self.code {
            None => true,
            Some(c) => {
                (2000..3000).contains(&c) || (1152..=1161).contains(&c) || matches!(c, 1040 | 1053 | 1317 | 3169)
            }
        }
    }

    /// 期待するエラーが、番号か SQLSTATE のどちらかで一致するか。
    pub fn is(&self, expected: &str) -> bool {
        self.code.is_some_and(|c| c.to_string() == expected) || self.sqlstate.as_deref() == Some(expected)
    }

    /// `ERROR 1062 (23000) at line 3: Duplicate entry ...` で始まる行を読む。
    pub fn from_stderr(stderr: &str) -> Self {
        Self::all_from_stderr(stderr).into_iter().next().unwrap_or_else(|| {
            let s = stderr.trim();
            Self::other(if s.is_empty() { "mysql exited" } else { s })
        })
    }

    /// `ERROR` で始まる行をすべて読む（`--force` で流したとき）。
    pub(crate) fn all_from_stderr(stderr: &str) -> Vec<Self> {
        stderr.lines().filter_map(|l| l.strip_prefix("ERROR ")).map(Self::from_line).collect()
    }

    fn from_line(line: &str) -> Self {
        let (code, rest) = line.split_once(' ').unwrap_or((line, ""));
        let sqlstate = rest.strip_prefix('(').and_then(|r| r.get(..5)).map(str::to_owned);
        let message = rest.split_once(": ").map_or(rest, |(_, m)| m);
        // 行番号は最初の `: ` より前にある（`1062 (23000) at line 3: ...`、`at line 1: Unknown command ...`）
        let head = line.split_once(": ").map_or("", |(h, _)| h);
        let line_no = head.find("at line ").and_then(|k| head[k + "at line ".len()..].trim().parse().ok());
        match code.parse() {
            Ok(code) => Self { message: message.to_owned(), code: Some(code), sqlstate, line: line_no },
            Err(_) => Self { line: line_no, ..Self::other(format!("ERROR {line}")) },
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

/// このプロセスが開くコネクションすべてに付ける印。
fn process_mark() -> &'static str {
    static MARK: OnceLock<String> = OnceLock::new();
    MARK.get_or_init(random_hex)
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// base64（標準の字。コンテナの中の `base64 -d` で戻す）。
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * k)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// コマンドの終わり方と出力。
struct Output {
    ok: bool,
    timed_out: bool,
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

/// program を走らせ、input を標準入力に流す。timeout を過ぎたら止める（timed_out）。
/// own_group なら別のプロセスグループで走らせる（端末の Ctrl-C が、途中の `docker run` を殺さないように）。
fn command(
    program: &str,
    args: &[&str],
    input: Option<String>,
    timeout: Duration,
    own_group: bool,
) -> Result<Output, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    if own_group {
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    }
    #[cfg(not(unix))]
    let _ = own_group;
    let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
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
    Ok(Output { ok: status.is_some_and(|s| s.success()), timed_out: status.is_none(), stdout, stderr })
}

fn timed_out(what: &str, timeout: Duration) -> SqlError {
    SqlError::other(format!("docker {what} timed out after {}s (set {TIMEOUT_ENV} to wait longer)", timeout.as_secs()))
}

/// docker を走らせる。時間切れと、docker を走らせられないことはエラー。
fn docker_with(args: &[&str], input: Option<String>, timeout: Duration, own_group: bool) -> Result<Output, SqlError> {
    let what = args.first().unwrap_or(&"");
    let out = command("docker", args, input, timeout, own_group).map_err(SqlError::other)?;
    if out.timed_out { Err(timed_out(what, timeout)) } else { Ok(out) }
}

/// docker を走らせ、成功すれば標準出力を返す。
fn docker(args: &[&str], timeout: Duration) -> Result<String, SqlError> {
    let out = docker_with(args, None, timeout, false)?;
    if out.ok {
        Ok(out.stdout)
    } else {
        Err(SqlError::other(format!("docker {}: {}", args.first().unwrap_or(&""), out.stderr.trim())))
    }
}

/// このプロセスの持ち主としての素性。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Owner {
    host: String,
    pid: String,
    /// PID 名前空間の番号（`/proc/self/ns/pid`）。分からなければ空。
    pid_ns: String,
    /// カーネルの起動ごとの ID（`/proc/sys/kernel/random/boot_id`）。分からなければ空。
    boot: String,
}

impl Owner {
    fn me() -> Self {
        let read = |p: &str| std::fs::read_to_string(p).map(|s| s.trim().to_owned()).unwrap_or_default();
        let mut host = read("/proc/sys/kernel/hostname");
        if host.is_empty() {
            host = command("hostname", &[], None, Duration::from_secs(5), false)
                .map(|o| o.stdout.trim().to_owned())
                .unwrap_or_default();
        }
        let pid_ns = std::fs::read_link("/proc/self/ns/pid")
            .map(|l| l.to_string_lossy().chars().filter(char::is_ascii_digit).collect())
            .unwrap_or_default();
        Self { host, pid: std::process::id().to_string(), pid_ns, boot: read("/proc/sys/kernel/random/boot_id") }
    }

    /// ラベルの値（`ホスト名:プロセス ID:PID 名前空間:起動の ID`）。
    fn label(&self) -> String {
        format!("{}:{}:{}:{}", self.host, self.pid, self.pid_ns, self.boot)
    }

    /// ラベルの値を読む（古い版の `ホスト名:プロセス ID` も読む）。
    fn parse(label: &str) -> Self {
        let mut parts = label.split(':').map(str::to_owned);
        let mut next = || parts.next().unwrap_or_default();
        Self { host: next(), pid: next(), pid_ns: next(), boot: next() }
    }

    /// 持ち主のプロセスが生きているかを、このプロセスから確かめられるか（同じカーネルの同じ PID 名前空間）。
    fn comparable(&self, me: &Self) -> bool {
        !self.pid_ns.is_empty() && !self.boot.is_empty() && self.pid_ns == me.pid_ns && self.boot == me.boot
    }
}

/// 見えるプロセス（このプロセスを含む）の `AI_YAGNI_MYSQL` が指すコンテナの名前。
fn referenced_containers() -> Vec<String> {
    let mut out: Vec<String> = std::env::var(ENV).into_iter().filter(|v| !v.is_empty()).collect();
    let Ok(procs) = std::fs::read_dir("/proc") else { return out };
    let key = format!("{ENV}=");
    for p in procs.flatten() {
        if !p.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(env) = std::fs::read(p.path().join("environ")) {
            for var in env.split(|&b| b == 0) {
                if let Some(v) = String::from_utf8_lossy(var).strip_prefix(&key)
                    && !v.is_empty()
                    && !out.iter().any(|o| o == v)
                {
                    out.push(v.to_owned());
                }
            }
        }
    }
    out
}

/// 片付けてよいコンテナか。owner は持ち主のラベル、started は立てた時刻（UNIX 秒）。
/// 持ち主のプロセスが生きているか確かめられるなら、それだけで決める（生きていれば古くても残す）。
/// 確かめられなければ（別のホスト、別の PID 名前空間、古い版のラベル）、古さだけで決める。ラベルがなければ残す。
fn is_stale(owner: &str, started: &str, me: &Owner, now: u64, stale_after: u64) -> bool {
    if owner.is_empty() {
        return false;
    }
    let o = Owner::parse(owner);
    if o.comparable(me) {
        return o.pid.parse().is_ok_and(|p| !signals::alive(p));
    }
    started.parse::<u64>().is_ok_and(|s| now.saturating_sub(s) > stale_after)
}

/// 前に立てて止め損ねたコンテナを片付ける（ほかの検査が使っているかもしれないものは残す）。
fn reap_stale(timeout: Duration) {
    let format = format!("{{{{.Names}}}}\t{{{{.Label \"{OWNER_LABEL}\"}}}}\t{{{{.Label \"{STARTED_LABEL}\"}}}}");
    let filter = format!("label={LABEL}");
    let Ok(list) = docker(&["ps", "-a", "--filter", &filter, "--format", &format], timeout) else { return };
    let referenced = referenced_containers();
    let (me, now) = (Owner::me(), now_secs());
    let stale_after = env_number(STALE_ENV).unwrap_or(DEFAULT_STALE_MINUTES) * 60;
    for line in list.lines() {
        let mut cols = line.split('\t');
        let (name, owner, started) = (cols.next().unwrap_or(""), cols.next().unwrap_or(""), cols.next().unwrap_or(""));
        if !name.is_empty() && !referenced.iter().any(|r| r == name) && is_stale(owner, started, &me, now, stale_after)
        {
            let _ = docker(&["rm", "-f", name], timeout);
        }
    }
}

// ------------------------------------------------------------------- information_schema の写しの使い回し

type Cached = Arc<dyn Any + Send + Sync>;

/// (コンテナ, データベース, 型) ごとに 1 つ。そのデータベースへ何かを流したら捨てる。
static CACHE: Mutex<Vec<(String, String, TypeId, Cached)>> = Mutex::new(Vec::new());

fn invalidate(container: &str, database: Option<&str>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(c, d, _, _)| !(c == container && database.is_none_or(|db| db == d)));
}

/// db について計算したもの（information_schema の写しなど）を、その db へ次に何かを流すまで使い回す。
/// [`Db::run`]・[`Db::query`]・[`Server::run`] はどれも、流した先の写しを捨てる。
pub(crate) fn cached<T: Any + Send + Sync, E>(db: &Db<'_>, fetch: impl FnOnce() -> Result<T, E>) -> Result<Arc<T>, E> {
    let (container, database, ty) = (db.server.container.as_str(), db.database.as_str(), TypeId::of::<T>());
    let found = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(c, d, t, _)| c == container && d == database && *t == ty)
        .map(|(_, _, _, v)| v.clone());
    if let Some(v) = found.and_then(|v| v.downcast::<T>().ok()) {
        return Ok(v);
    }
    let v = Arc::new(fetch()?);
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).push((
        container.to_owned(),
        database.to_owned(),
        ty,
        v.clone() as Cached,
    ));
    Ok(v)
}

// ------------------------------------------------------------------------------------- サーバー

/// mysql につなぐ利用者。
struct Login<'a> {
    user: &'a str,
    password: &'a str,
}

const ROOT: Login<'static> = Login { user: "root", password: PASSWORD };

/// コンテナの中で、標準入力の 1 行ずつ（`データベース名か - <空白> base64 のスクリプト`）を、
/// 1 本ずつ別の mysql で流す。始める前に、その番号を標準エラーに書く。失敗したらそこで止める。
const EACH: &str = r#"t=$(mktemp) || exit 125
trap 'rm -f "$t"' EXIT
cat > "$t"
n=$1; shift
i=1
while [ "$i" -le "$n" ]; do
  line=$(sed -n "${i}p" "$t")
  db=${line%% *}
  echo "@@ai-yagni-step $i" >&2
  if [ "$db" = - ]; then
    printf '%s' "${line#* }" | base64 -d | mysql "$@" > /dev/null || exit $?
  else
    printf '%s' "${line#* }" | base64 -d | mysql "$@" -D "$db" > /dev/null || exit $?
  fi
  i=$((i+1))
done"#;

/// docker の中の MySQL サーバー。自分で立てたもの（owned）は drop で止める。
pub struct Server {
    container: String,
    owned: bool,
    timeout: Duration,
    /// 消すのを待っているデータベース（名前, シグナルの片付けの番号）。まとめて 1 回の docker exec で消す。
    doomed: Mutex<Vec<(String, u64)>>,
}

impl Server {
    fn new(container: String, owned: bool) -> Self {
        let timeout = env_number(TIMEOUT_ENV).map_or(DEFAULT_TIMEOUT, Duration::from_secs);
        Self { container, owned, timeout, doomed: Mutex::new(Vec::new()) }
    }

    /// 新しいサーバーを立て、つながるまで待つ。前に止め損ねたコンテナを先に片付ける。
    pub fn start() -> Result<Self, SqlError> {
        let server = Self::new(format!("ai-yagni-mysql-{}", random_hex()), true);
        reap_stale(server.timeout);
        let pwd = format!("MYSQL_ROOT_PASSWORD={PASSWORD}");
        let owner = format!("{OWNER_LABEL}={}", Owner::me().label());
        let started = format!("{STARTED_LABEL}={}", now_secs());
        signals::own(&server.container);
        // 途中でシグナルを受けたら、片付けはこれが終わるのを待つ（立ち上がったコンテナを取り残さない）
        let starting = signals::Starting::new();
        let run = docker_with(
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
            None,
            server.timeout,
            true,
        );
        drop(starting);
        match run {
            Ok(out) if out.ok => {}
            Ok(out) => return Err(SqlError::other(format!("docker run: {}", out.stderr.trim()))),
            Err(e) => return Err(e),
        }
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

    /// コンテナの中で mysql を走らせる `docker exec` の引数（mysql の引数は mysql の後ろに extra を足す）。
    /// mysql の引数だけが欲しいとき（EACH に渡す）は、`mysql` の位置を返す。
    fn mysql_args(&self, login: &Login<'_>, run: &str, extra: &[&str]) -> (Vec<String>, usize) {
        let mut args: Vec<String> =
            ["exec", "-i", "-e", &format!("MYSQL_PWD={}", login.password), &self.container].map(str::to_owned).to_vec();
        let at = args.len();
        args.extend(
            [
                "mysql",
                &format!("-u{}", login.user),
                "-h127.0.0.1",
                "--binary-mode",
                "--skip-reconnect",
                "--default-character-set=utf8mb4",
                &format!(
                    "--init-command=SET time_zone = '+00:00', @{RUN_MARK} = '{run}', @{OWNER_MARK} = '{}'",
                    process_mark()
                ),
            ]
            .map(str::to_owned),
        );
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        (args, at)
    }

    /// mysql を走らせる。時間を過ぎたら、印の付いたコネクションをサーバーの上で止める。
    fn exec(&self, args: &[String], input: Option<String>, run: &str) -> Result<Output, SqlError> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = command("docker", &args, input, self.timeout, false).map_err(SqlError::other)?;
        if out.timed_out {
            kill_marked(&self.container, RUN_MARK, run);
            return Err(timed_out(args.first().unwrap_or(&""), self.timeout));
        }
        Ok(out)
    }

    fn wait_ready(&self, timeout: Duration) -> Result<(), SqlError> {
        // 初期化中の一時サーバーは TCP を開かないので、127.0.0.1 で SELECT が通れば本番のサーバーが上がっている
        let until = Instant::now() + timeout;
        let (args, _) = self.mysql_args(&ROOT, "ready", &["-e", "SELECT 1"]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        loop {
            match docker(&args, self.timeout) {
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
        invalidate(&self.container, database);
        self.read(database, script)
    }

    /// run と同じだが、information_schema の写しを捨てない
    /// （読むだけか、使い回しているデータベースに触らないと分かっているスクリプトのため）。
    pub(crate) fn read(&self, database: Option<&str>, script: &str) -> Result<Vec<ResultSet>, SqlError> {
        let mut extra = vec!["--xml"];
        if let Some(db) = database {
            extra.extend(["-D", db]);
        }
        let run = random_hex();
        let (args, _) = self.mysql_args(&ROOT, &run, &extra);
        let out = self.exec(&args, Some(terminate(script)), &run)?;
        if !out.ok {
            return Err(SqlError::from_stderr(&out.stderr));
        }
        Ok(parse_result_sets(&out.stdout))
    }

    /// エラーで止まらずに最後まで流し（`--force`）、サーバーが返したエラーをすべて返す（行番号つき）。
    /// 接続の失敗など、サーバーの判定でないエラーが 1 つでもあれば Err。読むだけのスクリプト（PREPARE など）に使う。
    pub(crate) fn run_forced(
        &self,
        database: &str,
        script: &str,
        user: Option<(&str, &str)>,
    ) -> Result<Vec<SqlError>, SqlError> {
        let login = user.map_or(ROOT, |(user, password)| Login { user, password });
        let run = random_hex();
        let (args, _) = self.mysql_args(&login, &run, &["--force", "-D", database]);
        let out = self.exec(&args, Some(terminate(script)), &run)?;
        let errors = SqlError::all_from_stderr(&out.stderr);
        if let Some(e) = errors.iter().find(|e| e.is_other()) {
            return Err(e.clone());
        }
        if !out.ok && errors.is_empty() {
            return Err(SqlError::from_stderr(&out.stderr));
        }
        Ok(errors)
    }

    /// スクリプトを順に、1 本ずつ別のコネクションで流す（docker exec は 1 回）。各スクリプトは、
    /// データベース（None ならどこにもつながない）と中身。失敗したら、その番号とエラーを返してそこで止める。
    pub fn run_each(&self, steps: &[(Option<&str>, &str)]) -> Result<(), (usize, SqlError)> {
        for (db, _) in steps {
            invalidate(&self.container, *db);
        }
        self.run_each_keeping(steps)
    }

    /// run_each と同じだが、information_schema の写しを捨てない（呼ぶ側が捨てるものを決める）。
    fn run_each_keeping(&self, steps: &[(Option<&str>, &str)]) -> Result<(), (usize, SqlError)> {
        if steps.is_empty() {
            return Ok(());
        }
        let mut input = String::new();
        for (db, script) in steps {
            input.push_str(db.unwrap_or("-"));
            input.push(' ');
            input.push_str(&base64(terminate(script).as_bytes()));
            input.push('\n');
        }
        let run = random_hex();
        let (mut args, at) = self.mysql_args(&ROOT, &run, &[]);
        let mysql = args.split_off(at);
        args.extend(["sh", "-c", EACH, "sh", &steps.len().to_string()].map(str::to_owned));
        args.extend(mysql.into_iter().skip(1));
        let out = self.exec(&args, Some(input), &run).map_err(|e| (0, e))?;
        if out.ok {
            return Ok(());
        }
        // 最後に始めたスクリプトが失敗した（印がなければ、始める前に失敗した）
        let (step, rest) = match out.stderr.rfind("@@ai-yagni-step ") {
            Some(at) => {
                let rest = &out.stderr[at + "@@ai-yagni-step ".len()..];
                let (n, rest) = rest.split_once('\n').unwrap_or((rest, ""));
                (n.trim().parse::<usize>().map_or(0, |n| n.saturating_sub(1)), rest)
            }
            None => (0, out.stderr.as_str()),
        };
        Err((step, SqlError::from_stderr(rest)))
    }

    /// 新しい空のデータベースの名前を決める。
    fn database_name(prefix: &str) -> Result<String, SqlError> {
        let clean: String =
            prefix.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
        let clean: String = clean.chars().take(40).collect();
        let name = format!("{clean}_{}", random_hex());
        ident(&name).map_err(SqlError::other)?;
        Ok(name)
    }

    /// 新しい空のデータベースを作って名前を返す。
    pub fn create_database(&self, prefix: &str) -> Result<String, SqlError> {
        let name = Self::database_name(prefix)?;
        self.run(None, &format!("CREATE DATABASE {}", ident(&name).map_err(SqlError::other)?))?;
        Ok(name)
    }

    /// データベースを消す。表の固定（メタデータのロック）は短く待つ（止まらない問い合わせが残っていても、長く待たない）。
    pub fn drop_database(&self, name: &str) -> Result<(), SqlError> {
        self.run(None, &drop_script(name)?)?;
        Ok(())
    }

    /// データベースを後でまとめて消す（DOOMED_BATCH 個たまったときと、サーバーを drop したとき）。
    fn drop_later(&self, name: &str, cleanup: u64) {
        let full = {
            let mut doomed = self.doomed.lock().unwrap_or_else(|e| e.into_inner());
            doomed.push((name.to_owned(), cleanup));
            doomed.len() >= DOOMED_BATCH
        };
        if full {
            self.drop_doomed();
        }
    }

    /// 消すのを待っているデータベースを、1 回の docker exec で消す（1 つが失敗しても残りは消す）。
    fn drop_doomed(&self) {
        let doomed = std::mem::take(&mut *self.doomed.lock().unwrap_or_else(|e| e.into_inner()));
        if doomed.is_empty() {
            return;
        }
        let mut script = "SET SESSION lock_wait_timeout = 10;\n".to_owned();
        for (name, _) in &doomed {
            invalidate(&self.container, Some(name));
            if let Ok(db) = ident(name) {
                script.push_str(&format!("DROP DATABASE IF EXISTS {db};\n"));
            }
        }
        let run = random_hex();
        let (args, _) = self.mysql_args(&ROOT, &run, &["--force"]);
        let _ = self.exec(&args, Some(script), &run);
        for (_, cleanup) in doomed {
            signals::done(cleanup);
        }
    }
}

fn drop_script(name: &str) -> Result<String, SqlError> {
    Ok(format!(
        "SET SESSION lock_wait_timeout = 10;\nDROP DATABASE IF EXISTS {}",
        ident(name).map_err(SqlError::other)?
    ))
}

/// 印（ユーザー変数 var の値が value）の付いたコネクションを、サーバーの上で止める。
/// performance_schema が使えなければ何もしない。
fn kill_marked(container: &str, var: &str, value: &str) {
    let server = Server::attach(container).timeout(SHORT);
    let find = format!(
        "SELECT t.PROCESSLIST_ID AS id FROM performance_schema.user_variables_by_thread v \
         JOIN performance_schema.threads t ON t.THREAD_ID = v.THREAD_ID \
         WHERE v.VARIABLE_NAME = {} AND v.VARIABLE_VALUE = {} AND t.PROCESSLIST_ID <> CONNECTION_ID()",
        literal(var),
        literal(value)
    );
    let Ok(sets) = server.read(None, &find) else { return };
    let ids: Vec<String> = sets
        .iter()
        .flat_map(|s| &s.rows)
        .filter_map(|r| r.try_get("id").ok().flatten().map(str::to_owned))
        .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if !ids.is_empty() {
        let kills: Vec<String> = ids.iter().map(|id| format!("KILL {id};")).collect();
        let run = random_hex();
        let (args, _) = server.mysql_args(&ROOT, &run, &["--force"]);
        let _ = server.exec(&args, Some(kills.join("\n")), &run);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // 立てたコンテナは丸ごと消すので、データベースを先に消すことはない
        if !self.owned {
            self.drop_doomed();
        }
        if self.owned {
            let _ = docker(&["rm", "-f", &self.container], self.timeout);
            signals::disown(&self.container);
        }
    }
}

/// MySQL を 1 つ立て、`AI_YAGNI_MYSQL` にそのコンテナ名を入れて args のコマンドを走らせ、終わったら止める。
/// 終了コードを返す。テスト全体で 1 つのサーバーを共有するために、利用者の小さな bin から呼ぶ（`with-mysql cargo test`）。
/// SIGINT か SIGTERM を受けたら、そのシグナルをコマンドに渡し、コマンドが終わってから（長くても 60 秒待って）
/// 立てたコンテナを止めて終わる。2 度目のシグナルでは待たずに終わる。
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
    let status = Command::new(cmd).args(rest).env(ENV, server.container()).spawn().and_then(|mut child| {
        let pid = child.id();
        signals::child(pid);
        let status = child.wait();
        signals::child_done(pid);
        status
    });
    drop(server);
    match status {
        Ok(_) if signals::received() != 0 => 128 + signals::received(),
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
        one(self.run(script)?)
    }

    /// query と同じだが、information_schema の写しを捨てない（読むだけと分かっているスクリプトのため）。
    pub(crate) fn read(&self, script: &str) -> Result<Vec<Row>, SqlError> {
        one(self.server.read(Some(&self.database), script)?)
    }
}

fn one(mut sets: Vec<ResultSet>) -> Result<Vec<Row>, SqlError> {
    match sets.len() {
        1 => Ok(sets.remove(0).rows),
        n => Err(SqlError::other(format!("expected exactly one result, got {n}"))),
    }
}

/// 新しく作り、drop で消すデータベース。シグナルで終わるときも消す（SIGINT・SIGTERM の片付けを入れていれば）。
///
/// 消すのはまとめて行う（いくつかたまったときと、[`Server`] を drop したとき）。docker exec の数を減らすため。
pub struct Scratch<'s> {
    pub db: Db<'s>,
    cleanup: u64,
}

impl<'s> Scratch<'s> {
    pub fn create(server: &'s Server, label: &str) -> Result<Self, SqlError> {
        let database = Server::database_name(label)?;
        let scratch = Self::register(server, database);
        invalidate(server.container(), Some(&scratch.db.database));
        server.read(None, &format!("CREATE DATABASE {}", ident(&scratch.db.database).map_err(SqlError::other)?))?;
        Ok(scratch)
    }

    /// 新しいデータベースを作り、scripts を順に 1 本ずつ別のコネクションで流す（docker exec は 1 回）。
    /// 作れなければ Err。作れたら、失敗したスクリプトがあればその番号とエラーを添えて返す（そこで止める）。
    pub fn build(
        server: &'s Server,
        label: &str,
        scripts: &[&str],
    ) -> Result<(Self, Option<(usize, SqlError)>), SqlError> {
        let database = Server::database_name(label)?;
        let scratch = Self::register(server, database);
        let create = format!("CREATE DATABASE {}", ident(&scratch.db.database).map_err(SqlError::other)?);
        let mut steps = vec![(None, create.as_str())];
        steps.extend(scripts.iter().map(|s| (Some(scratch.db.database.as_str()), *s)));
        // 新しい名前のデータベースを作るだけの最初の文は、ほかのデータベースの写しに触らない
        invalidate(server.container(), Some(&scratch.db.database));
        match server.run_each_keeping(&steps) {
            Ok(()) => Ok((scratch, None)),
            Err((0, e)) => Err(e),
            Err((i, e)) => Ok((scratch, Some((i - 1, e)))),
        }
    }

    fn register(server: &'s Server, database: String) -> Self {
        let cleanup = drop_script(&database).map_or(0, |sql| signals::on_exit(server.container(), &sql));
        Self { db: Db { server, database }, cleanup }
    }
}

impl Drop for Scratch<'_> {
    fn drop(&mut self) {
        self.db.server.drop_later(&self.db.database, self.cleanup);
    }
}

/// 使い捨ての DB の利用者（drop で消す。シグナルで終わるときも消す）。権限は呼んだ側が付ける。
pub(crate) struct ScratchUser<'s> {
    server: &'s Server,
    pub name: String,
    pub password: String,
    cleanup: u64,
}

impl<'s> ScratchUser<'s> {
    pub(crate) fn create(server: &'s Server) -> Result<Self, SqlError> {
        let (name, password) = (format!("ayg_{}", random_hex()), format!("{}{}", random_hex(), random_hex()));
        let cleanup = signals::on_exit(server.container(), &format!("DROP USER IF EXISTS '{name}'@'%'"));
        let user = Self { server, name, password, cleanup };
        // 利用者を作っても、データベースの information_schema の写しは変わらない
        server.read(None, &format!("CREATE USER '{}'@'%' IDENTIFIED BY '{}'", user.name, user.password))?;
        Ok(user)
    }
}

impl Drop for ScratchUser<'_> {
    fn drop(&mut self) {
        let _ = self.server.read(None, &format!("DROP USER IF EXISTS '{}'@'%'", self.name));
        signals::done(self.cleanup);
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
        assert_eq!(e.line, Some(3));
        assert!(e.is("1062") && e.is("23000") && !e.is("45000") && !e.is_other());
        let s = SqlError::from_stderr("ERROR 1644 (45000): transfers are append-only");
        assert_eq!(s.message, "transfers are append-only");
        assert_eq!(s.line, None);
        assert_eq!(SqlError::from_stderr("").message, "mysql exited");
        assert!(SqlError::from_stderr("").is_other());
        let cli = SqlError::from_stderr("ERROR at line 1: Unknown command '\\g'.");
        assert!(cli.is_other() && cli.line == Some(1), "{cli:?}");
    }

    #[test]
    fn client_and_connection_errors_are_not_verdicts() {
        for code in [2013, 2006, 2002, 1040, 1053, 1158, 1317, 3169] {
            let e = SqlError::from_stderr(&format!("ERROR {code} (HY000): Lost connection to MySQL server"));
            assert!(e.is_other(), "{code}");
        }
        assert!(!SqlError::from_stderr("ERROR 3819 (HY000): Check constraint 'c' is violated.").is_other());
    }

    #[test]
    fn only_a_line_that_starts_with_error_is_read_as_the_error() {
        let e = SqlError::from_stderr(
            "mysql: [Warning] see ERROR 9 here\nERROR 1146 (42S02) at line 1: Table 'x' doesn't exist\n",
        );
        assert_eq!(e.code, Some(1146));
        assert!(SqlError::from_stderr("docker: Error response: no ERROR 1 here").is_other());
        assert_eq!(
            SqlError::all_from_stderr("ERROR 1054 (42S22) at line 1: a\nERROR 1146 (42S02) at line 3: b\n").len(),
            2
        );
    }

    #[test]
    fn random_names_differ() {
        assert_ne!(random_hex(), random_hex());
        assert_eq!(random_hex().len(), 10);
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("SELECT 'é';\n".as_bytes()), "U0VMRUNUICfDqSc7Cg==");
    }

    #[test]
    fn stale_containers_are_reaped_only_when_it_is_safe() {
        let me = Owner { host: "box".into(), pid: "100".into(), pid_ns: "4026531836".into(), boot: "b-1".into() };
        let now = 10_000;
        let label = |host: &str, pid: &str, ns: &str, boot: &str| {
            Owner { host: host.into(), pid: pid.into(), pid_ns: ns.into(), boot: boot.into() }.label()
        };
        // 同じ名前空間で、持ち主のプロセスがもういない
        assert!(is_stale(&label("box", "2147483646", "4026531836", "b-1"), "9990", &me, now, 600));
        // 同じ名前空間で、持ち主が生きている（このプロセス）: 古くても残す
        let alive = label("box", &std::process::id().to_string(), "4026531836", "b-1");
        assert!(!is_stale(&alive, "1000", &me, now, 600));
        // 同じホスト名でも、別の PID 名前空間（別のコンテナ）なら生きているか分からない: 若ければ残す
        assert!(!is_stale(&label("box", "2147483646", "4026532000", "b-1"), "9990", &me, now, 600));
        // 別のカーネル（別のマシン）も同じ
        assert!(!is_stale(&label("box", "2147483646", "4026531836", "b-2"), "9990", &me, now, 600));
        // 確かめられなくても、十分に古ければ片付ける
        assert!(is_stale(&label("other", "1", "4026532000", "b-2"), "1000", &me, now, 600));
        // 古い版のラベル（ホスト名:プロセス ID）は古さだけで決める
        assert!(!is_stale("box:2147483646", "9990", &me, now, 600));
        assert!(is_stale("box:2147483646", "1000", &me, now, 600));
        // ラベルのないものは、いつ誰が立てたか分からないので残す
        assert!(!is_stale("", "", &me, now, 600));
        assert_eq!(Owner::parse(&me.label()), me);
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped() {
        let started = Instant::now();
        assert!(command("sleep", &["30"], None, Duration::from_millis(200), false).unwrap().timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
        let out = command("cat", &[], Some("hello".to_owned()), Duration::from_secs(10), false).unwrap();
        assert!(out.ok && out.stdout == "hello");
    }
}
