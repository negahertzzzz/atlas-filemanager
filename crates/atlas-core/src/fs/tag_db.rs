// reason: runs on the dedicated SQLite blocking worker, never the gpui render
// thread; the sync-fs ban targets UI-foreground blocking.
#![allow(clippy::disallowed_methods)]
//! SQLite-backed tag store.
//!
//! Owns the only `rusqlite::Connection` on a dedicated blocking worker (rusqlite is
//! `!Sync` and SQL is blocking I/O). The UI never touches the connection: it sends
//! [`TagDbCmd`] over a channel and receives answers back through the shared
//! `BgResult` channel, drained on the UI thread. Writes are fire-and-forget; reads
//! reply asynchronously. This keeps the 60fps loop free of SQL latency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use atlas_model::app_state::BgResult;
use atlas_model::tag_cache::{TagDef, TagId};

/// Commands accepted by the tag-db worker.
pub enum TagDbCmd {
    // --- tag definitions (each re-sends the full def list afterwards) ---
    CreateTag {
        name: String,
        color: [u8; 3],
        profile: Option<String>,
    },
    /// Create a definition and assign it to the picker targets in one transaction.
    CreateAndAssignTag {
        name: String,
        color: [u8; 3],
        profile: Option<String>,
        paths: Vec<PathBuf>,
    },
    RenameTag {
        tag: TagId,
        name: String,
    },
    DeleteTag(TagId),
    // --- associations (fire-and-forget) ---
    AssignTag {
        tag: TagId,
        paths: Vec<PathBuf>,
    },
    UnassignTag {
        tag: TagId,
        paths: Vec<PathBuf>,
    },
    /// Re-key associations after an in-app move/rename (file or directory subtree).
    RenamePath {
        old: PathBuf,
        new: PathBuf,
    },
    /// Remove plaintext path references after a verified private-folder migration.
    ForgetSubtree(PathBuf),
    // --- reads (reply via BgResult) ---
    QueryTagsForPaths(Vec<PathBuf>),
    QueryPathsForTag(TagId),
}

/// Spawn the tag-db worker. `migration` is imported only on a brand-new database
/// (used to carry over tags that previously lived in profile config). Returns the
/// channel the UI uses to talk to the worker.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    db_path: PathBuf,
    migration: Vec<TagDef>,
    bg_tx: flume::Sender<BgResult>,
) -> flume::Sender<TagDbCmd> {
    let (tx, rx) = flume::unbounded::<TagDbCmd>();
    rt.spawn_blocking(move || {
        let conn = match open(&db_path) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!("tag_db: failed to open {db_path:?}: {e}");
                return;
            }
        };
        if let Err(e) = migrate_initial(&conn, &migration) {
            tracing::error!("tag_db: initial migration failed: {e}");
        }
        if let Err(e) = send_defs(&conn, &bg_tx) {
            tracing::error!("tag_db: initial def load failed: {e}");
        }
        while let Ok(cmd) = rx.recv() {
            if let Err(e) = handle(&conn, cmd, &bg_tx) {
                tracing::error!("tag_db: command failed: {e}");
            }
        }
    });
    tx
}

fn open(db_path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS tags (
            id      INTEGER PRIMARY KEY,
            name    TEXT    NOT NULL,
            color_r INTEGER NOT NULL,
            color_g INTEGER NOT NULL,
            color_b INTEGER NOT NULL,
            profile TEXT,
            UNIQUE(name, profile)
         );
         CREATE TABLE IF NOT EXISTS file_tags (
            path   TEXT    NOT NULL,
            tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (path, tag_id)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_file_tags_tag ON file_tags(tag_id);",
    )?;
    Ok(conn)
}

fn migrate_initial(conn: &Connection, migration: &[TagDef]) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
    if count > 0 || migration.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO tags (name, color_r, color_g, color_b, profile)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for def in migration {
            stmt.execute(params![
                def.name,
                def.color[0],
                def.color[1],
                def.color[2],
                def.profile,
            ])?;
        }
    }
    tx.commit()
}

fn handle(
    conn: &Connection,
    cmd: TagDbCmd,
    bg_tx: &flume::Sender<BgResult>,
) -> rusqlite::Result<()> {
    match cmd {
        TagDbCmd::CreateTag {
            name,
            color,
            profile,
        } => {
            create_tag_with_paths(conn, &name, color, profile.as_deref(), &[])?;
            send_defs(conn, bg_tx)?;
        }
        TagDbCmd::CreateAndAssignTag {
            name,
            color,
            profile,
            mut paths,
        } => {
            paths.retain(|path| !crate::vault::live::is_private(path));
            let tag = create_tag_with_paths(conn, &name, color, profile.as_deref(), &paths)?;
            send_defs(conn, bg_tx)?;
            if !paths.is_empty() {
                let _ = bg_tx.send(BgResult::TagAssignmentComplete {
                    tag,
                    paths,
                    result: Ok(()),
                });
            }
        }
        TagDbCmd::DeleteTag(id) => {
            conn.execute("DELETE FROM tags WHERE id = ?1", [id.0])?;
            send_defs(conn, bg_tx)?;
        }
        TagDbCmd::RenameTag { tag, name } => {
            conn.execute(
                "UPDATE OR IGNORE tags SET name = ?1 WHERE id = ?2",
                params![name, tag.0],
            )?;
            send_defs(conn, bg_tx)?;
        }
        TagDbCmd::AssignTag { tag, mut paths } => {
            paths.retain(|path| !crate::vault::live::is_private(path));
            let result = (|| -> rusqlite::Result<()> {
                let tx = conn.unchecked_transaction()?;
                {
                    let mut stmt = tx.prepare(
                        "INSERT OR IGNORE INTO file_tags (path, tag_id) VALUES (?1, ?2)",
                    )?;
                    for p in &paths {
                        stmt.execute(params![path_key(p), tag.0])?;
                    }
                }
                tx.commit()
            })();
            let _ = bg_tx.send(BgResult::TagAssignmentComplete {
                tag,
                paths,
                result: result.as_ref().map(|_| ()).map_err(ToString::to_string),
            });
            result?;
        }
        TagDbCmd::UnassignTag { tag, paths } => {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt =
                    tx.prepare("DELETE FROM file_tags WHERE path = ?1 AND tag_id = ?2")?;
                for p in &paths {
                    stmt.execute(params![path_key(p), tag.0])?;
                }
            }
            tx.commit()?;
        }
        TagDbCmd::RenamePath { old, new } => {
            if crate::vault::live::is_private(&new) {
                return Ok(());
            }
            let old_key = path_key(&old);
            let new_key = path_key(&new);
            // The path itself (a tagged file, or the moved directory if it was tagged).
            conn.execute(
                "UPDATE OR IGNORE file_tags SET path = ?2 WHERE path = ?1",
                params![old_key, new_key],
            )?;
            // Tagged descendants when a directory subtree is moved.
            conn.execute(
                "UPDATE OR IGNORE file_tags
                 SET path = ?2 || substr(path, length(?1) + 1)
                 WHERE path LIKE ?1 || '/%'",
                params![old_key, new_key],
            )?;
        }
        TagDbCmd::ForgetSubtree(path) => {
            conn.execute_batch("PRAGMA secure_delete = ON;")?;
            let key = path_key(&path);
            conn.execute("DELETE FROM file_tags WHERE path = ?1 OR substr(path, 1, length(?1) + 1) = ?1 || '/'", [key])?;
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        TagDbCmd::QueryTagsForPaths(paths) => {
            let mut map: HashMap<PathBuf, Vec<TagId>> = HashMap::with_capacity(paths.len());
            let mut stmt = conn.prepare("SELECT tag_id FROM file_tags WHERE path = ?1")?;
            for p in paths {
                let ids = stmt
                    .query_map([path_key(&p)], |row| Ok(TagId(row.get(0)?)))?
                    .filter_map(Result::ok)
                    .collect();
                map.insert(p, ids);
            }
            let _ = bg_tx.send(BgResult::TagsForPaths { tags: map });
        }
        TagDbCmd::QueryPathsForTag(tag) => {
            let mut stmt = conn.prepare("SELECT path FROM file_tags WHERE tag_id = ?1")?;
            let paths = stmt
                .query_map([tag.0], |row| {
                    let s: String = row.get(0)?;
                    Ok(PathBuf::from(s))
                })?
                .filter_map(Result::ok)
                .collect();
            let _ = bg_tx.send(BgResult::PathsForTag { tag, paths });
        }
    }
    Ok(())
}

fn create_tag_with_paths(
    conn: &Connection,
    name: &str,
    color: [u8; 3],
    profile: Option<&str>,
    paths: &[PathBuf],
) -> rusqlite::Result<TagId> {
    let tx = conn.unchecked_transaction()?;
    // SQLite's UNIQUE(name, profile) permits repeated NULL profiles. Match global
    // definitions explicitly so an existing global tag is reused as well.
    tx.execute(
        "INSERT OR IGNORE INTO tags (name, color_r, color_g, color_b, profile)
         SELECT ?1, ?2, ?3, ?4, ?5
         WHERE NOT EXISTS (SELECT 1 FROM tags WHERE name = ?1 AND profile IS ?5)",
        params![name, color[0], color[1], color[2], profile],
    )?;
    let tag = tx.query_row(
        "SELECT id FROM tags WHERE name = ?1 AND profile IS ?2 ORDER BY id LIMIT 1",
        params![name, profile],
        |row| Ok(TagId(row.get(0)?)),
    )?;
    {
        let mut statement =
            tx.prepare("INSERT OR IGNORE INTO file_tags (path, tag_id) VALUES (?1, ?2)")?;
        for path in paths {
            statement.execute(params![path_key(path), tag.0])?;
        }
    }
    tx.commit()?;
    Ok(tag)
}

fn send_defs(conn: &Connection, bg_tx: &flume::Sender<BgResult>) -> rusqlite::Result<()> {
    let defs = load_defs(conn)?;
    let _ = bg_tx.send(BgResult::TagDefinitions(defs));
    Ok(())
}

fn load_defs(conn: &Connection) -> rusqlite::Result<Vec<TagDef>> {
    let mut stmt = conn
        .prepare("SELECT id, name, color_r, color_g, color_b, profile FROM tags ORDER BY name")?;
    let defs = stmt
        .query_map([], |row| {
            Ok(TagDef {
                id: TagId(row.get(0)?),
                name: row.get(1)?,
                color: [row.get(2)?, row.get(3)?, row.get(4)?],
                profile: row.get(5)?,
            })
        })?
        .filter_map(Result::ok)
        .collect();
    Ok(defs)
}

/// Associations are keyed by the file's absolute path string. We deliberately do not
/// canonicalize (which would resolve symlinks and require the file to exist): every
/// path the UI sends comes from `DirEntry::path()`, so they are already absolute and
/// consistent between write and read.
fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_from_picker_assigns_the_captured_targets_and_reuses_global_definitions() {
        let conn = open(Path::new(":memory:")).unwrap();
        let (tx, rx) = flume::unbounded();
        let paths = vec![
            PathBuf::from("images/first.png"),
            PathBuf::from("images/second.png"),
        ];
        for _ in 0..2 {
            handle(
                &conn,
                TagDbCmd::CreateAndAssignTag {
                    name: "Art".into(),
                    color: [120, 80, 240],
                    profile: None,
                    paths: paths.clone(),
                },
                &tx,
            )
            .unwrap();
        }
        let definitions = load_defs(&conn).unwrap();
        assert_eq!(definitions.len(), 1);
        let tag = definitions[0].id;
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM file_tags", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.into_iter().any(|event| matches!(event,
            BgResult::TagAssignmentComplete { tag: assigned, paths: targets, result: Ok(()) }
                if assigned == tag && targets == paths
        )));
        handle(&conn, TagDbCmd::QueryTagsForPaths(paths.clone()), &tx).unwrap();
        let BgResult::TagsForPaths { tags } = rx.recv().unwrap() else {
            panic!("Expected file tags");
        };
        assert!(paths.iter().all(|path| tags.get(path) == Some(&vec![tag])));
    }

    #[test]
    fn sidebar_creation_creates_only_a_definition() {
        let conn = open(Path::new(":memory:")).unwrap();
        let (tx, rx) = flume::unbounded();
        handle(
            &conn,
            TagDbCmd::CreateTag {
                name: "Unassigned".into(),
                color: [1, 2, 3],
                profile: Some("Main".into()),
            },
            &tx,
        )
        .unwrap();
        assert_eq!(load_defs(&conn).unwrap().len(), 1);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM file_tags", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(!rx
            .try_iter()
            .any(|event| matches!(event, BgResult::TagAssignmentComplete { .. })));
    }

    #[test]
    fn failed_picker_assignment_rolls_back_the_new_definition_and_all_targets() {
        let conn = open(Path::new(":memory:")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_second_target BEFORE INSERT ON file_tags
            WHEN NEW.path = 'second.png' BEGIN SELECT RAISE(ABORT, 'Rejected target'); END;",
        )
        .unwrap();
        let (tx, rx) = flume::unbounded();
        assert!(handle(
            &conn,
            TagDbCmd::CreateAndAssignTag {
                name: "Art".into(),
                color: [1, 2, 3],
                profile: None,
                paths: vec![PathBuf::from("first.png"), PathBuf::from("second.png")],
            },
            &tx
        )
        .is_err());
        assert!(load_defs(&conn).unwrap().is_empty());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM file_tags", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(rx.try_iter().next().is_none());
    }

    #[test]
    fn assignment_acknowledges_commit_and_failed_transactions_do_not_assign() {
        let conn = open(Path::new(":memory:")).unwrap();
        let (tx, rx) = flume::unbounded();
        handle(
            &conn,
            TagDbCmd::CreateTag {
                name: "Test".into(),
                color: [1, 2, 3],
                profile: None,
            },
            &tx,
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM tags", [], |row| row.get(0))
            .unwrap();
        let paths = vec![PathBuf::from("source/one"), PathBuf::from("source/two")];
        handle(
            &conn,
            TagDbCmd::AssignTag {
                tag: TagId(id),
                paths: paths.clone(),
            },
            &tx,
        )
        .unwrap();
        assert!(rx.try_iter().any(|event| matches!(event, BgResult::TagAssignmentComplete { tag, result: Ok(()), .. } if tag == TagId(id))));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM file_tags", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let invalid = TagId(id + 99);
        assert!(handle(
            &conn,
            TagDbCmd::AssignTag {
                tag: invalid,
                paths
            },
            &tx
        )
        .is_err());
        assert!(rx.try_iter().any(|event| matches!(event, BgResult::TagAssignmentComplete { tag, result: Err(_), .. } if tag == invalid)));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM file_tags", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
