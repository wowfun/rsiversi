use super::source::Batch;
use super::{
    ApiError, Arc, CancellationToken, Coverage, Hit, Mutex, PathBuf, Reply, Result, Scope, check,
    invalid,
};
use rsi_agent_session_protocol::ReferenceSource;
use rsi_history_api::Cursor;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

const MAXIMUM_PAGES: i64 = 1024 * 1024 * 1024 / 4096;
struct Database {
    connection: Connection,
    generation: String,
    _lease: File,
}
/// `SQLite` lives only behind retained owner work, including blocking completion.
#[derive(Debug)]
pub(super) struct Cache {
    inner: Arc<Mutex<Option<Database>>>,
}
impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryDatabase").finish_non_exhaustive()
    }
}
pub(super) fn key(scope: &Scope) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(scope).expect("typed scope"),
    ))
}
fn sql(error: rusqlite::Error) -> ApiError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DiskFull
                    | rusqlite::ErrorCode::OperationInterrupted
                    | rusqlite::ErrorCode::DatabaseBusy
                    | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            ApiError::Capacity
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            invalid(error)
        }
        _ => ApiError::Backend(error.to_string()),
    }
}
fn generation() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(invalid)?;
    Ok(hex::encode(bytes))
}
fn initial(source: ReferenceSource) -> Coverage {
    Coverage {
        source,
        indexed_through: "0".into(),
        observed_through: "0".into(),
        omissions: "0".into(),
        has_more: true,
    }
}
fn load(connection: &Connection, key: &str, source: ReferenceSource) -> Result<Coverage> {
    let row: Option<(i64, Option<String>)> = connection.query_row(
        "SELECT length(CAST(coverage AS BLOB)),CASE WHEN length(CAST(coverage AS BLOB)) BETWEEN 1 AND 8192 THEN coverage END FROM sources WHERE key=?1", [key], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(sql)?;
    let Some((length, bytes)) = row else {
        return Ok(initial(source));
    };
    if !(1..=8192).contains(&length) {
        return Err(invalid("history coverage exceeds limit"));
    }
    let bytes = bytes.ok_or_else(|| invalid("invalid history coverage"))?;
    let coverage: Coverage = serde_json::from_str(&bytes).map_err(invalid)?;
    coverage.validate()?;
    Ok(if coverage.source == source {
        coverage
    } else {
        initial(source)
    })
}
fn budget(connection: &Connection, stop: CancellationToken) -> Result<()> {
    let mut steps = 0;
    connection
        .progress_handler(
            1000,
            Some(move || {
                steps += 1000;
                stop.is_cancelled() || steps > 4_000_000
            }),
        )
        .map_err(invalid)
}
fn open(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql)?;
    budget(&connection, CancellationToken::new())?;
    connection.execute_batch("PRAGMA journal_mode=DELETE;PRAGMA temp_store=MEMORY;PRAGMA cache_size=-8192;PRAGMA page_size=4096;").map_err(sql)?;
    connection
        .pragma_update(None, "max_page_count", MAXIMUM_PAGES)
        .map_err(sql)?;
    Ok(connection)
}
fn schema(connection: &Connection) -> Result<String> {
    connection.execute_batch("CREATE TABLE meta(generation TEXT NOT NULL);CREATE TABLE sources(key TEXT PRIMARY KEY,coverage TEXT NOT NULL CHECK(length(coverage)<=8192));CREATE VIRTUAL TABLE documents USING fts5(body,source,metadata UNINDEXED,tokenize='unicode61');PRAGMA user_version=2;").map_err(invalid)?;
    let generation = generation()?;
    connection
        .execute("INSERT INTO meta VALUES (?1)", [&generation])
        .map_err(invalid)?;
    Ok(generation)
}
fn valid(connection: &Connection) -> Result<String> {
    let page_size: u32 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(sql)?;
    if page_size != 4096 {
        return Err(invalid("unsupported cache page size"));
    }
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sql)?;
    if version != 2 {
        return Err(invalid("obsolete history cache"));
    }
    // Inspect only bounded schema metadata, never the FTS contents on startup.
    let expected = Connection::open_in_memory().map_err(sql)?;
    schema(&expected)?;
    let layout = |database: &Connection| -> Result<Vec<(String, String)>> {
        database
            .prepare("SELECT name,coalesce(sql,'') FROM sqlite_master ORDER BY name LIMIT 32")
            .map_err(sql)?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(sql)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql)
    };
    if layout(connection)? != layout(&expected)? {
        return Err(invalid("obsolete history cache schema"));
    }
    let generations = connection.prepare("SELECT CASE WHEN length(CAST(generation AS BLOB))=32 THEN generation END FROM meta LIMIT 2").map_err(sql)?
        .query_map([], |row| row.get::<_, Option<String>>(0)).map_err(sql)?
        .collect::<std::result::Result<Vec<_>, _>>().map_err(sql)?;
    let [Some(generation)] = generations.as_slice() else {
        return Err(invalid("invalid cache generation record"));
    };
    let generation = generation.clone();
    if generation.len() != 32
        || !generation
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("invalid cache generation"));
    }
    Ok(generation)
}
fn source_match(key: &str) -> String {
    format!("source : \"{}\"", key.replace('"', "\"\""))
}
fn clear_source(db: &mut Database, key: &str, stop: &CancellationToken) -> Result<()> {
    let next = generation()?;
    let tx = db.connection.transaction().map_err(sql)?;
    tx.execute("DELETE FROM sources WHERE key=?1", [key])
        .map_err(sql)?;
    tx.execute("UPDATE meta SET generation=?1", [&next])
        .map_err(sql)?;
    tx.commit().map_err(sql)?;
    db.generation = next;
    loop {
        check(stop)?;
        budget(&db.connection, stop.clone())?;
        let tx = db.connection.transaction().map_err(sql)?;
        let deleted = tx.execute(
            "DELETE FROM documents WHERE rowid IN (SELECT rowid FROM documents WHERE documents MATCH ?1 LIMIT 128)",
            [source_match(key)],
        ).map_err(sql)?;
        tx.commit().map_err(sql)?;
        if deleted == 0 {
            return Ok(());
        }
    }
}
fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("native flag"));
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(invalid("history cache file is not private regular storage"));
    }
    let file = options.open(path).map_err(invalid)?;
    let metadata = file.metadata().map_err(invalid)?;
    if !metadata.is_file() {
        return Err(invalid("history cache requires regular files"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let named = std::fs::symlink_metadata(path).map_err(invalid)?;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || named.dev() != metadata.dev()
            || named.ino() != metadata.ino()
        {
            return Err(invalid("history cache file ownership mismatch"));
        }
    }
    Ok(file)
}
fn directory(root: &Path) -> Result<File> {
    if !root.is_absolute() {
        return Err(invalid("history cache directory must be absolute"));
    }
    #[cfg(unix)]
    let _directory =
        rsi_files_native_fs::create_absolute_directory_no_follow(root).map_err(invalid)?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(root).map_err(invalid)?;
    let metadata = std::fs::symlink_metadata(root).map_err(invalid)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("history cache must be a dedicated directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(invalid("history directory must be private"));
        }
    }
    for item in std::fs::read_dir(root).map_err(invalid)? {
        let item = item.map_err(invalid)?;
        if ![".writer.lock", "history.sqlite3", "history.sqlite3-journal"]
            .iter()
            .any(|name| item.file_name() == *name)
            || !item.file_type().map_err(invalid)?.is_file()
        {
            return Err(invalid("history directory contains unrelated files"));
        }
    }
    let lease = private_file(&root.join(".writer.lock"))?;
    lease.try_lock().map_err(|_| ApiError::Capacity)?;
    Ok(lease)
}
impl Cache {
    pub async fn open(directory_path: PathBuf) -> Result<Self> {
        let database = tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            let directory_path =
                rsi_files_native_fs::resolve_absolute_root_alias(&directory_path, true)
                    .map_err(invalid)?;
            let lease = directory(&directory_path)?;
            let path = directory_path.join("history.sqlite3");
            let file = private_file(&path)?;
            if file.metadata().map_err(invalid)?.len() > 1024 * 1024 * 1024 {
                return Err(invalid("history cache exceeds 1 GiB"));
            }
            drop(file);
            // Only this dedicated reconstructible cache is removed, under its lease.
            let existing = open(&path).and_then(|connection| {
                valid(&connection).map(|generation| (connection, generation))
            });
            let (connection, generation) = match existing {
                Ok(pair) => pair,
                Err(ApiError::Invalid(_)) => {
                    for name in ["history.sqlite3-journal", "history.sqlite3"] {
                        let path = directory_path.join(name);
                        if path.exists() {
                            drop(private_file(&path)?);
                            std::fs::remove_file(path).map_err(invalid)?;
                        }
                    }
                    drop(private_file(&path)?);
                    let connection = open(&path)?;
                    let generation = schema(&connection)?;
                    (connection, generation)
                }
                Err(error) => return Err(error),
            };
            Ok::<_, ApiError>(Database {
                connection,
                generation,
                _lease: lease,
            })
        })
        .await
        .map_err(invalid)??;
        Ok(Self {
            inner: Arc::new(Mutex::new(Some(database))),
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        stop: CancellationToken,
        work: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            check(&stop)?;
            let mut guard = inner.lock().expect("history cache");
            let database = guard.as_mut().ok_or(ApiError::ShuttingDown)?;
            budget(&database.connection, stop.clone())?;
            let result = work(database);
            database
                .connection
                .progress_handler(0, None::<fn() -> bool>)
                .map_err(invalid)?;
            check(&stop)?;
            result
        })
        .await
        .map_err(invalid)?
    }
    pub async fn close(&self) {
        let inner = self.inner.clone();
        let _ =
            tokio::task::spawn_blocking(move || drop(inner.lock().expect("history cache").take()))
                .await;
    }
    pub async fn coverage(
        &self,
        key: String,
        source: ReferenceSource,
        stop: CancellationToken,
    ) -> Result<Coverage> {
        self.run(stop, move |db| load(&db.connection, &key, source))
            .await
    }
    pub async fn advance(
        &self,
        key: String,
        previous: Coverage,
        batch: Batch,
        stop: CancellationToken,
    ) -> Result<Reply> {
        self.run(stop.clone(),move|db|{
        let actual=load(&db.connection,&key,previous.source.clone())?;if actual!=previous{return Err(invalid("history progress changed"));}
        if previous.indexed_through=="0" && db.connection.query_row("SELECT rowid FROM documents WHERE documents MATCH ?1 LIMIT 1",[source_match(&key)],|row|row.get::<_,i64>(0)).optional().map_err(sql)?.is_some() {clear_source(db,&key,&stop)?;budget(&db.connection,stop.clone())?;}
        let tx=db.connection.transaction().map_err(sql)?;
        for document in batch.documents {tx.execute("INSERT INTO documents(body,source,metadata) VALUES (?1,?2,?3)",params![document.text,key,serde_json::to_string(&document.hit).map_err(invalid)?]).map_err(sql)?;}
        let coverage=Coverage{source:previous.source,indexed_through:batch.through.to_string(),observed_through:batch.horizon.to_string(),omissions:rsi_history_api::decimal(&previous.omissions)?.checked_add(batch.omissions).ok_or_else(||invalid("history omission count exhausted"))?.to_string(),has_more:batch.has_more};coverage.validate()?;
        let count:i64=tx.query_row("SELECT count(*) FROM sources",[],|row|row.get(0)).map_err(invalid)?;
        if count>=4096 && tx.query_row("SELECT 1 FROM sources WHERE key=?1",[&key],|_|Ok(())).optional().map_err(invalid)?.is_none(){return Err(ApiError::Capacity);}
        tx.execute("INSERT INTO sources(key,coverage) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET coverage=excluded.coverage",params![key,serde_json::to_string(&coverage).map_err(invalid)?]).map_err(sql)?;
        tx.commit().map_err(sql)?;Ok(Reply::Coverage{coverage})
    }).await
    }
    pub async fn rebuild(
        &self,
        key: String,
        source: ReferenceSource,
        stop: CancellationToken,
    ) -> Result<Reply> {
        self.run(stop.clone(), move |db| {
            clear_source(db, &key, &stop)?;
            Ok(Reply::Coverage {
                coverage: initial(source),
            })
        })
        .await
    }
    pub async fn search(
        &self,
        key: String,
        scope: Scope,
        query: String,
        after: Option<Cursor>,
        coverage: Coverage,
        stop: CancellationToken,
    ) -> Result<Reply> {
        self.run(stop,move|db|{
        let start=if let Some(after)=after{if after.generation!=db.generation{return Err(invalid("history cursor expired after rebuild"));}i64::try_from(rsi_history_api::decimal(&after.after)?).map_err(invalid)?}else{0};
        if coverage.indexed_through=="0"{return Ok(Reply::Hits{coverage,hits:vec![],next:None});}
        // Quote every token; callers cannot inject operators, columns or MATCH syntax.
        let lexical=query.split_whitespace().map(|word|format!("\"{}\"",word.replace('"',"\"\""))).collect::<Vec<_>>().join(" AND ");
        let lexical=format!("{} AND body : ({lexical})", source_match(&key));
        let mut statement=db.connection.prepare("SELECT rowid,length(CAST(metadata AS BLOB)) FROM documents WHERE documents MATCH ?1 AND rowid>?2 ORDER BY rowid LIMIT 65").map_err(sql)?;
        let coordinates=statement.query_map(params![lexical,start],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?))).map_err(invalid)?.collect::<std::result::Result<Vec<_>,_>>().map_err(invalid)?;
        let mut more=coordinates.len()>64;let mut hits=Vec::new();let mut last=0;let mut retained=32*1024;
        let mut selected = Vec::new();
        for (row, length) in coordinates.into_iter().take(64) {
            if row<=0 || length<=0 || length>16384 { return Err(invalid("invalid cached hit length")); }
            if retained+length>256*1024 { more=true; break; }
            retained+=length; selected.push(row);
        }
        if !selected.is_empty() {
            // Admit lengths first, then materialize precisely those rows in one query.
            let placeholders = vec!["?"; selected.len()].join(",");
            let mut statement = db.connection.prepare(&format!("SELECT rowid,metadata FROM documents WHERE rowid IN ({placeholders}) ORDER BY rowid")).map_err(sql)?;
            let mut rows = statement.query(rusqlite::params_from_iter(&selected)).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                let coordinate:i64=row.get(0).map_err(sql)?;
                if selected.get(hits.len()) != Some(&coordinate) { return Err(invalid("cached result coordinates changed")); }
                let bytes:String=row.get(1).map_err(sql)?;
                let hit:Hit=serde_json::from_str(&bytes).map_err(invalid)?;
                hit.validate()?;
                if hit.source!=coverage.source { return Err(invalid("cached source identity mismatch")); }
                hits.push(hit);last=coordinate;
            }
            if hits.len()!=selected.len() { return Err(invalid("cached result disappeared")); }
        }
        let next=more.then(||Cursor{generation:db.generation.clone(),query,scope,after:last.to_string()});
        // Worst-case JSON escaping can exceed a page before its 64-hit ceiling.
        let reply=Reply::Hits{coverage,hits,next};
        if serde_json::to_vec(&reply).map_err(invalid)?.len()>256*1024 {return Err(invalid("history result byte budget exceeded"));}
        Ok(reply)
    }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn source_search_and_rebuild_do_not_scan_unrelated_documents() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path().join("cache")).await.unwrap();
        let source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "history".into(),
            epoch: 1,
        };
        let scope:Scope=serde_json::from_value(serde_json::json!({"workspace":"b".repeat(64),"conversation":{"kind":"external","id":"history"}})).unwrap();
        let key = key(&scope);
        let hit:Hit=serde_json::from_value(serde_json::json!({"source":source,"original":{"record":{"sequence":"1","kind":"human","content_index":0},"through_seq":"1","start":0,"end":6,"text_sha256":"a".repeat(64),"scanned_bytes":256},"preview":"needle"})).unwrap();
        cache.run(CancellationToken::new(),{let key=key.clone();move |db| {
            db.connection.progress_handler(0,None::<fn()->bool>).unwrap();
            db.connection.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<500000) INSERT INTO documents(body,source,metadata) SELECT 'needle','unrelated','{}' FROM n",[]).map_err(sql)?;
            db.connection.execute("INSERT INTO documents(body,source,metadata) VALUES('needle',?1,?2)",params![key,serde_json::to_string(&hit).unwrap()]).map_err(sql)?;Ok(())
        }}).await.unwrap();
        let coverage = Coverage {
            source: source.clone(),
            indexed_through: "1".into(),
            observed_through: "1".into(),
            omissions: "0".into(),
            has_more: false,
        };
        let searched = cache
            .search(
                key.clone(),
                scope,
                "needle".into(),
                None,
                coverage,
                CancellationToken::new(),
            )
            .await;
        let rebuilt = cache.rebuild(key, source, CancellationToken::new()).await;
        assert!(
            matches!(searched,Ok(Reply::Hits{ref hits,..}) if hits.len()==1),
            "source query: {searched:?}"
        );
        rebuilt.expect("one-source reset cannot scan unrelated sources");
        cache
            .run(CancellationToken::new(), |db| {
                db.connection
                    .progress_handler(0, None::<fn() -> bool>)
                    .unwrap();
                let count: i64 = db
                    .connection
                    .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
                    .map_err(sql)?;
                assert_eq!(count, 500_000);
                Ok(())
            })
            .await
            .unwrap();
        cache.close().await;
    }

    #[tokio::test]
    async fn interrupted_reset_keeps_committed_cleanup_and_can_finish_on_retry() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path().join("cache")).await.unwrap();
        let source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "history".into(),
            epoch: 1,
        };
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        cache.run(CancellationToken::new(),move |db| {
            db.connection.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO documents(body,source,metadata) SELECT 'text','target','{}' FROM n",[]).map_err(sql)?;
            let mut deletes=0;
            db.connection.update_hook(Some(move |action:rusqlite::hooks::Action,_:&str,table:&str,_:i64| {
                if action==rusqlite::hooks::Action::SQLITE_DELETE && table=="documents_content" {deletes+=1;if deletes==256 {cancelled.cancel();}}
            })).map_err(sql)?;
            Ok(())
        }).await.unwrap();
        assert!(
            cache
                .rebuild("target".into(), source.clone(), stop)
                .await
                .is_err()
        );
        cache
            .run(CancellationToken::new(), |db| {
                db.connection
                    .update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)
                    .map_err(sql)?;
                let count: i64 = db
                    .connection
                    .query_row("SELECT count(*) FROM documents", [], |row| row.get(0))
                    .map_err(sql)?;
                assert!(
                    count > 0 && count < 10000,
                    "a cancelled reset preserves completed deletion batches: {count}"
                );
                Ok(())
            })
            .await
            .unwrap();
        cache
            .rebuild("target".into(), source, CancellationToken::new())
            .await
            .unwrap();
        cache
            .run(CancellationToken::new(), |db| {
                assert_eq!(
                    db.connection
                        .query_row("SELECT count(*) FROM documents", [], |row| row
                            .get::<_, i64>(0))
                        .map_err(sql)?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
        cache.close().await;
    }
    #[tokio::test]
    async fn escaped_previews_page_without_losing_later_hits() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path().join("cache")).await.unwrap();
        let source = ReferenceSource::Observed {
            owner: "acp".into(),
            id: "history".into(),
            epoch: 1,
        };
        let scope: Scope = serde_json::from_value(
            json!({"workspace":"b".repeat(64),"conversation":{"kind":"external","id":"history"}}),
        )
        .unwrap();
        let mut hit:Hit=serde_json::from_value(json!({"source":source,"original":{"record":{"sequence":"1","kind":"human","content_index":0},"through_seq":"70","start":0,"end":2048,"text_sha256":"a".repeat(64),"scanned_bytes":16384},"preview":"\u{1}".repeat(2048)})).unwrap();
        let key = key(&scope);
        cache.run(CancellationToken::new(), {let key=key.clone();move |db| {
            for sequence in 1..=70 { hit.original.record.sequence=sequence; hit.validate()?; db.connection.execute("INSERT INTO documents(body,source,metadata) VALUES('needle',?1,?2)", params![key,serde_json::to_string(&hit).unwrap()]).map_err(sql)?; }
            Ok(())
        }}).await.unwrap();
        let coverage = Coverage {
            source,
            indexed_through: "70".into(),
            observed_through: "70".into(),
            omissions: "0".into(),
            has_more: false,
        };
        let mut cursor = None;
        let mut sequences = Vec::new();
        let mut pages = 0;
        loop {
            let request = rsi_history_api::Request::Search {
                scope: scope.clone(),
                query: "needle".into(),
                after: cursor.clone(),
            };
            let reply = cache
                .search(
                    key.clone(),
                    scope.clone(),
                    "needle".into(),
                    cursor,
                    coverage.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            rsi_history_api::validate_reply(&request, &reply).unwrap();
            let Reply::Hits { hits, next, .. } = reply else {
                panic!("hits")
            };
            assert!(!hits.is_empty());
            sequences.extend(hits.into_iter().map(|hit| hit.original.record.sequence));
            pages += 1;
            cursor = next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(sequences, (1..=70).collect::<Vec<_>>());
        assert!(pages > 1);
        cache.close().await;
    }
    #[test]
    fn startup_validation_does_not_scan_indexed_documents() {
        let connection = Connection::open_in_memory().unwrap();
        let generation = schema(&connection).unwrap();
        connection.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO documents(body,source,metadata) SELECT 'searchable text', 's', '{}' FROM n", []).unwrap();
        let steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = steps.clone();
        connection
            .progress_handler(
                100,
                Some(move || observed.fetch_add(100, std::sync::atomic::Ordering::Relaxed) >= 2000),
            )
            .unwrap();
        assert_eq!(valid(&connection).unwrap(), generation);
        assert!(steps.load(std::sync::atomic::Ordering::Relaxed) <= 2000);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_waiter_keeps_dispatched_sqlite_work_and_lease_until_actual_completion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cache");
        let cache = Arc::new(Cache::open(path.clone()).await.unwrap());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let worker = tokio::spawn({
            let cache = cache.clone();
            async move {
                cache
                    .run(CancellationToken::new(), move |_| {
                        entered.send(()).unwrap();
                        blocked.recv().unwrap();
                        Ok(())
                    })
                    .await
            }
        });
        ready.await.unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        let close = tokio::spawn({
            let cache = cache.clone();
            async move { cache.close().await }
        });
        assert!(
            Cache::open(path.clone()).await.is_err(),
            "actual blocking work still owns the database lease"
        );
        assert!(!close.is_finished());
        release.send(()).unwrap();
        close.await.unwrap();
        Cache::open(path).await.unwrap().close().await;
    }
    #[tokio::test]
    async fn sqlite_work_is_interrupted_and_failed_transactions_keep_coverage() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path().join("cache")).await.unwrap();
        let error = cache.run(CancellationToken::new(), |db| {
            db.connection.query_row("WITH RECURSIVE work(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM work WHERE n<1000000000) SELECT sum(n) FROM work", [], |row| row.get::<_,i64>(0)).map_err(invalid)
        }).await.unwrap_err();
        assert!(error.to_string().contains("interrupted"), "{error}");
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            cache
                .run(cancelled, |_| -> Result<()> {
                    panic!("cancelled work entered SQLite")
                })
                .await,
            Err(ApiError::ShuttingDown)
        ));
        cache.run(CancellationToken::new(), |db| {
            let pages:i64=db.connection.pragma_query_value(None,"page_count",|row|row.get(0)).map_err(invalid)?;
            db.connection.pragma_update(None,"max_page_count",pages+2).map_err(invalid)?;
            let tx=db.connection.transaction().map_err(sql)?;
            tx.execute("INSERT INTO sources VALUES('quota','pending')",[]).map_err(invalid)?;
            assert!(tx.execute("INSERT INTO documents(body,source,metadata) VALUES(hex(zeroblob(1048576)),'quota','{}')",[]).is_err());
            drop(tx);
            let count:i64=db.connection.query_row("SELECT count(*) FROM sources WHERE key='quota'",[],|row|row.get(0)).map_err(invalid)?;
            assert_eq!(count,0,"failed index mutation must not advance durable coverage");
            Ok(())
        }).await.unwrap();
        cache.close().await;
    }
    #[tokio::test]
    async fn corrupt_cache_rebuilds_under_exclusive_lease() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cache");
        let cache = Cache::open(path.clone()).await.unwrap();
        assert!(Cache::open(path.clone()).await.is_err());
        cache.close().await;
        std::fs::write(path.join("history.sqlite3"), b"broken database").unwrap();
        let rebuilt = Cache::open(path.clone()).await.unwrap();
        rebuilt.close().await;
        let connection = Connection::open(path.join("history.sqlite3")).unwrap();
        assert!(valid(&connection).is_ok());
    }
}
