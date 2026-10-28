//! Loads the two JSONL bundles produced by Elysian SDE into PostgreSQL.
//!
//! * `eve-online-client-static-data-<build>-jsonl` -> schema `sde_native`
//! * `eve-online-static-data-<build>-jsonl`        -> schema `sde_compat`
//!
//! Every file becomes a lossless `(row_no, key, data jsonb)` table. A typed SQL
//! script (`db/sde_typed.sql`) then derives the indexed `sde.*` tables the
//! server actually queries.
//!
//! Usage: sde-import --sde <dir> [--typed-sql <file>] [--jobs N]
//! Connection string comes from `DATABASE_URL`.

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures_util::SinkExt;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_postgres::{Client, NoTls};

const COPY_CHUNK: usize = 1 << 20;

struct Args {
    sde: PathBuf,
    typed_sql: Option<PathBuf>,
    jobs: usize,
    db_url: String,
}

fn parse_args() -> Result<Args> {
    let mut sde = None;
    let mut typed_sql = None;
    let mut jobs = 4;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sde" => sde = it.next().map(PathBuf::from),
            "--typed-sql" => typed_sql = it.next().map(PathBuf::from),
            "--jobs" => jobs = it.next().context("--jobs needs a value")?.parse()?,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(Args {
        sde: sde.context("--sde <dir> is required")?,
        typed_sql,
        jobs: jobs.max(1),
        db_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
    })
}

/// One JSONL file destined for one table.
struct Job {
    schema: &'static str,
    table: String,
    path: PathBuf,
}

/// File stem -> safe lower-case identifier (`mapObjects__celestials` -> `mapobjects__celestials`).
fn table_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut name: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, 't');
    }
    name
}

fn collect_jobs(root: &Path) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(root).with_context(|| format!("reading {}", root.display()))? {
        let dir = entry?.path();
        let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if !dir.is_dir() || !name.ends_with("-jsonl") {
            continue;
        }
        let schema = if name.starts_with("eve-online-client-static-data-") {
            "sde_native"
        } else if name.starts_with("eve-online-static-data-") {
            "sde_compat"
        } else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        files.sort();
        for path in files {
            let table = table_name(&path);
            if !seen.insert(table.clone()) {
                bail!("table name collision in {schema}: {table} ({})", path.display());
            }
            jobs.push(Job { schema, table, path });
        }
    }
    if jobs.is_empty() {
        bail!("no *-jsonl bundles found under {}", root.display());
    }
    // Largest first so the long tail (mapMoons, localization) starts early.
    jobs.sort_by_key(|j| std::cmp::Reverse(std::fs::metadata(&j.path).map(|m| m.len()).unwrap_or(0)));
    Ok(jobs)
}

/// Make Python `json.dumps` output acceptable to PostgreSQL `jsonb`:
/// bare `NaN` / `Infinity` / `-Infinity` become `null`, and `\u0000`
/// escapes (rejected by jsonb) are dropped. Only ASCII is rewritten, so the
/// result stays valid UTF-8.
fn sanitize(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains("NaN") && !line.contains("Infinity") && !line.contains("\\u0000") {
        return line.into();
    }
    let b = line.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                if b[i..].starts_with(b"\\u0000") {
                    i += 6;
                    continue;
                }
                out.extend_from_slice(&b[i..(i + 2).min(b.len())]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
        } else if let Some(tok) = [&b"-Infinity"[..], b"Infinity", b"NaN"].into_iter().find(|t| b[i..].starts_with(t)) {
            out.extend_from_slice(b"null");
            i += tok.len();
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).expect("sanitize only rewrites ASCII").into()
}

fn key_of(v: &serde_json::Value) -> Option<String> {
    match v.get("_key")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

fn csv_field(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for &b in s.as_bytes() {
        if b == b'"' {
            out.push(b'"');
        }
        out.push(b);
    }
    out.push(b'"');
}

async fn load_table(client: &mut Client, job: &Job) -> Result<u64> {
    let fq = format!("\"{}\".\"{}\"", job.schema, job.table);
    let tx = client.transaction().await?;
    tx.batch_execute(&format!(
        "DROP TABLE IF EXISTS {fq};
         CREATE TABLE {fq} (row_no integer PRIMARY KEY, key text, data jsonb NOT NULL);"
    ))
    .await?;

    let sink = tx
        .copy_in::<_, Bytes>(&format!("COPY {fq} (row_no, key, data) FROM STDIN (FORMAT csv)"))
        .await?;
    futures_util::pin_mut!(sink);

    let text = tokio::fs::read_to_string(&job.path)
        .await
        .with_context(|| format!("reading {}", job.path.display()))?;
    let mut buf = Vec::with_capacity(COPY_CHUNK + 4096);
    let mut rows = 0u64;
    for (n, raw) in text.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let line = sanitize(raw);
        let value: serde_json::Value = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: invalid JSON", job.path.display(), n + 1))?;
        buf.extend_from_slice(rows.to_string().as_bytes());
        buf.push(b',');
        if let Some(k) = key_of(&value) {
            csv_field(&mut buf, &k);
        }
        buf.push(b',');
        csv_field(&mut buf, &line);
        buf.push(b'\n');
        rows += 1;
        if buf.len() >= COPY_CHUNK {
            sink.send(Bytes::from(std::mem::take(&mut buf))).await?;
        }
    }
    if !buf.is_empty() {
        sink.send(Bytes::from(buf)).await?;
    }
    let copied = sink.as_mut().finish().await?;
    if copied != rows {
        bail!("{fq}: wrote {rows} rows but COPY reported {copied}");
    }
    tx.batch_execute(&format!("CREATE INDEX ON {fq} (key);")).await?;
    tx.commit().await?;
    Ok(rows)
}

async fn connect(url: &str) -> Result<Client> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await.context("connecting to PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("postgres connection error: {e}");
        }
    });
    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args()?;
    let started = Instant::now();
    let jobs = collect_jobs(&args.sde)?;
    println!("{} tables to load from {}", jobs.len(), args.sde.display());

    let admin = connect(&args.db_url).await?;
    admin
        .batch_execute("CREATE SCHEMA IF NOT EXISTS sde_native; CREATE SCHEMA IF NOT EXISTS sde_compat;")
        .await?;

    let permits = Arc::new(Semaphore::new(args.jobs));
    let mut handles = Vec::new();
    for job in jobs {
        let permit = permits.clone().acquire_owned().await?;
        let url = args.db_url.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let t = Instant::now();
            let mut client = connect(&url).await?;
            let rows = load_table(&mut client, &job)
                .await
                .with_context(|| format!("loading {}.{}", job.schema, job.table))?;
            println!("  {}.{:<48} {:>9} rows  {:>6.1}s", job.schema, job.table, rows, t.elapsed().as_secs_f64());
            anyhow::Ok(rows)
        }));
    }
    let mut total = 0;
    let mut failed = 0;
    for h in handles {
        match h.await? {
            Ok(n) => total += n,
            Err(e) => {
                failed += 1;
                eprintln!("ERROR: {e:#}");
            }
        }
    }
    if failed > 0 {
        bail!("{failed} table(s) failed to load");
    }
    println!("raw load: {total} rows in {:.1}s", started.elapsed().as_secs_f64());

    if let Some(sql_path) = args.typed_sql {
        let t = Instant::now();
        let sql = std::fs::read_to_string(&sql_path).with_context(|| format!("reading {}", sql_path.display()))?;
        admin.batch_execute(&sql).await.with_context(|| format!("running {}", sql_path.display()))?;
        for row in admin
            .query(
                "SELECT c.relname, c.reltuples::bigint FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = 'sde' AND c.relkind = 'r' ORDER BY c.relname",
                &[],
            )
            .await?
        {
            println!("  sde.{:<32} {:>9} rows", row.get::<_, String>(0), row.get::<_, i64>(1));
        }
        println!("typed schema built in {:.1}s", t.elapsed().as_secs_f64());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_non_finite_outside_strings() {
        assert_eq!(sanitize(r#"{"a": NaN, "b": -Infinity, "c": Infinity}"#), r#"{"a": null, "b": null, "c": null}"#);
        assert_eq!(sanitize(r#"{"NaN": "Infinity NaN"}"#), r#"{"NaN": "Infinity NaN"}"#);
    }

    #[test]
    fn sanitize_drops_nul_escapes_only() {
        assert_eq!(sanitize(r#"{"a": "x\u0000y", "b": "\\u0000"}"#), r#"{"a": "xy", "b": "\\u0000"}"#);
    }

    #[test]
    fn table_names_are_safe() {
        assert_eq!(table_name(Path::new("mapObjects__celestials.jsonl")), "mapobjects__celestials");
        assert_eq!(table_name(Path::new("localization_fsd_en-us.jsonl")), "localization_fsd_en_us");
        assert_eq!(table_name(Path::new("_sde.jsonl")), "_sde");
    }
}
