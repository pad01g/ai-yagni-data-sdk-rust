//! 層0: 本物の MySQL を docker のコンテナとして立て、コンテナの中の mysql CLI で話す。
//!
//! - データは tmpfs に置く（使い捨て。速さのため）。ポートは公開しない。話す経路は `docker exec` だけ。
//! - 1 つのサーバーの上に、検査ごとに新しいデータベースを作って分ける（コンテナを立て直すより速い）。
//! - 環境変数 `AI_YAGNI_MYSQL` にコンテナ名があれば、それにつなぐ（立てて止めるのは呼んだ側の仕事）。
//! - スクリプト 1 つを mysql の 1 プロセス（＝1 本のコネクション）で流す。mysql はエラーに当たると終了し、
//!   その時点のトランザクションはサーバーが巻き戻す。終了をそのまま [`SqlError`] として返す。
mod sqltext;
mod xml;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub use sqltext::{ident, literal, terminate};
pub use xml::{ResultSet, Row, parse_result_sets};

pub const IMAGE: &str = "mysql:8.4.6";
pub const ENV: &str = "AI_YAGNI_MYSQL";
/// ポートを公開しない使い捨てのサーバーのパスワード。
const PASSWORD: &str = "datafutures";
const LABEL: &str = "ai-yagni.sdk=mysql";

/// mysql が返したエラー。CLI 自体の失敗なら code と sqlstate は None。
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

    /// 期待するエラーが、番号か SQLSTATE のどちらかで一致するか。
    pub fn is(&self, expected: &str) -> bool {
        self.code.is_some_and(|c| c.to_string() == expected) || self.sqlstate.as_deref() == Some(expected)
    }

    /// `ERROR 1062 (23000) at line 3: Duplicate entry ...` を読む。
    pub fn from_stderr(stderr: &str) -> Self {
        let Some(at) = stderr.find("ERROR ") else {
            let s = stderr.trim();
            return Self::other(if s.is_empty() { "mysql exited" } else { s });
        };
        let line = stderr[at + 6..].lines().next().unwrap_or("");
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

fn random_hex() -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos());
    format!("{:010x}", h.finish() & 0xff_ffff_ffff)
}

fn docker(args: &[&str]) -> Result<String, SqlError> {
    let out = Command::new("docker").args(args).output().map_err(|e| SqlError::other(format!("docker: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(SqlError::other(format!(
            "docker {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// docker の中の MySQL サーバー。自分で立てたもの（owned）は drop で止める。
pub struct Server {
    container: String,
    owned: bool,
}

impl Server {
    /// 新しいサーバーを立て、つながるまで待つ。
    pub fn start() -> Result<Self, SqlError> {
        let name = format!("ai-yagni-mysql-{}", random_hex());
        let pwd = format!("MYSQL_ROOT_PASSWORD={PASSWORD}");
        docker(&[
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "--label",
            LABEL,
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
        ])?;
        let server = Self { container: name, owned: true };
        server.wait_ready(Duration::from_secs(180))?;
        Ok(server)
    }

    /// 既に立っているサーバー（コンテナ名）につなぐ。止めない。
    pub fn attach(container: impl Into<String>) -> Self {
        Self { container: container.into(), owned: false }
    }

    /// `AI_YAGNI_MYSQL` のサーバーがあればそれに、なければ新しく立てる。
    pub fn obtain() -> Result<Self, SqlError> {
        match std::env::var(ENV) {
            Ok(name) if !name.is_empty() => Ok(Self::attach(name)),
            _ => Self::start(),
        }
    }

    pub fn container(&self) -> &str {
        &self.container
    }

    fn wait_ready(&self, timeout: Duration) -> Result<(), SqlError> {
        // 初期化中の一時サーバーは TCP を開かないので、127.0.0.1 で SELECT が通れば本番のサーバーが上がっている
        let until = Instant::now() + timeout;
        let pwd = format!("MYSQL_PWD={PASSWORD}");
        loop {
            let r = docker(&["exec", "-e", &pwd, &self.container, "mysql", "-uroot", "-h127.0.0.1", "-e", "SELECT 1"]);
            match r {
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
        let mut args = vec![
            "exec",
            "-i",
            "-e",
            &pwd,
            &self.container,
            "mysql",
            "-uroot",
            "-h127.0.0.1",
            "--xml",
            "--default-character-set=utf8mb4",
        ];
        if let Some(db) = database {
            args.extend(["-D", db]);
        }
        let mut child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SqlError::other(format!("docker: {e}")))?;
        let input = format!("SET time_zone = '+00:00';\n{}", terminate(script));
        let mut stdin = child.stdin.take().expect("stdin is piped");
        // 出力を読みながら書く（大きな結果でパイプが詰まらないように）
        let writer = thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let out = child.wait_with_output().map_err(|e| SqlError::other(format!("docker: {e}")))?;
        let _ = writer.join();
        if !out.status.success() {
            return Err(SqlError::from_stderr(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(parse_result_sets(&String::from_utf8_lossy(&out.stdout)))
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
            let _ = docker(&["rm", "-f", &self.container]);
        }
    }
}

/// MySQL を 1 つ立て、`AI_YAGNI_MYSQL` にそのコンテナ名を入れて args のコマンドを走らせ、終わったら止める。
/// 終了コードを返す。テスト全体で 1 つのサーバーを共有するために、利用者の小さな bin から呼ぶ（`with-mysql cargo test`）。
pub fn with_server(args: &[String]) -> i32 {
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("usage: with-mysql <command> [args...]");
        return 2;
    };
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

    /// 最後の結果の行（結果を返す文がなければ空）。
    pub fn query(&self, script: &str) -> Result<Vec<Row>, SqlError> {
        Ok(self.run(script)?.pop().map(|s| s.rows).unwrap_or_default())
    }

    pub fn one(&self, script: &str) -> Result<Option<Row>, SqlError> {
        Ok(self.query(script)?.into_iter().next())
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
        assert!(e.is("1062") && e.is("23000") && !e.is("45000"));
        let s = SqlError::from_stderr("ERROR 1644 (45000): transfers are append-only");
        assert_eq!(s.message, "transfers are append-only");
        assert_eq!(SqlError::from_stderr("").message, "mysql exited");
    }
}
