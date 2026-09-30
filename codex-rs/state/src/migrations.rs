use std::borrow::Cow;

use sqlx::AssertSqlSafe;
use sqlx::Connection;
use sqlx::Row;
use sqlx::SqlSafeStr;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;
use sqlx_sqlite::SqlitePool;

pub(crate) static STATE_MIGRATOR: Migrator = sqlx_macros::migrate!("./migrations");
pub(crate) static LOGS_MIGRATOR: Migrator = sqlx_macros::migrate!("./logs_migrations");
pub(crate) static GOALS_MIGRATOR: Migrator = sqlx_macros::migrate!("./goals_migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx_macros::migrate!("./memory_migrations");
pub(crate) static QUEUE_MIGRATOR: Migrator = sqlx_macros::migrate!("./queue_migrations");
pub(crate) static THREAD_HISTORY_MIGRATOR: Migrator =
    sqlx_macros::migrate!("./thread_history_migrations");

/// Allow an older Codex binary to open a database that has already been
/// migrated by a newer binary running in parallel.
///
/// We intentionally ignore applied migration versions that are newer than the
/// embedded migration set. Known migration versions are still validated by
/// checksum, so this only relaxes the "database is ahead of me" case.
fn runtime_migrator(base: &'static Migrator) -> Migrator {
    Migrator {
        migrations: Cow::Borrowed(base.migrations.as_ref()),
        ignore_missing: true,
        locking: base.locking,
        no_tx: base.no_tx,
        table_name: base.table_name.clone(),
        create_schemas: base.create_schemas.clone(),
    }
}

pub(crate) fn runtime_state_migrator() -> Migrator {
    runtime_migrator(&STATE_MIGRATOR)
}

pub(crate) fn runtime_logs_migrator() -> Migrator {
    runtime_migrator(&LOGS_MIGRATOR)
}

pub(crate) fn runtime_goals_migrator() -> Migrator {
    runtime_migrator(&GOALS_MIGRATOR)
}

pub(crate) fn runtime_memories_migrator() -> Migrator {
    runtime_migrator(&MEMORIES_MIGRATOR)
}

pub(crate) fn runtime_queue_migrator() -> Migrator {
    runtime_migrator(&QUEUE_MIGRATOR)
}

// The paginated history projector will call this when it takes ownership of opening the database.
#[allow(dead_code)]
pub(crate) fn runtime_thread_history_migrator() -> Migrator {
    runtime_migrator(&THREAD_HISTORY_MIGRATOR)
}

pub(crate) async fn repair_legacy_recency_migration_version(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    let Some(recency_migration) = migrator
        .migrations
        .iter()
        .find(|migration| migration.version == 39)
    else {
        return Ok(());
    };
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        return Ok(());
    }

    let legacy_recency_needs_repair = sqlx::query_scalar::<_, i64>(
        r#"
SELECT 1
FROM _sqlx_migrations
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(38_i64)
    .bind(recency_migration.checksum.as_ref())
    .bind(recency_migration.version)
    .fetch_optional(pool)
    .await?
    .is_some();
    if !legacy_recency_needs_repair {
        return Ok(());
    }

    sqlx::query(
        r#"
UPDATE _sqlx_migrations
SET version = ?, description = ?
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
        "#,
    )
    .bind(recency_migration.version)
    .bind(recency_migration.description.as_ref())
    .bind(38_i64)
    .bind(recency_migration.checksum.as_ref())
    .bind(recency_migration.version)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn run_migrations_with_line_ending_compatibility(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    match migrator.run(pool).await {
        Ok(()) => Ok(()),
        Err(error @ MigrateError::VersionMismatch(_)) => {
            if repair_line_ending_migration_checksums(pool, migrator).await? == 0 {
                return Err(error.into());
            }
            migrator.run(pool).await.map_err(anyhow::Error::from)
        }
        Err(error) => Err(error.into()),
    }
}

async fn repair_line_ending_migration_checksums(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<usize> {
    if migrator.table_name.as_ref() != "_sqlx_migrations" {
        return Ok(0);
    }

    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
    let applied = sqlx::query(
        "SELECT version, checksum FROM _sqlx_migrations WHERE success = 1 ORDER BY version",
    )
    .fetch_all(&mut *transaction)
    .await?;
    let mut repairs = Vec::new();

    for row in applied {
        let version = row.get::<i64, _>("version");
        let stored_checksum = row.get::<Vec<u8>, _>("checksum");
        let Some(migration) = migrator
            .migrations
            .iter()
            .find(|migration| migration.version == version)
        else {
            continue;
        };
        if stored_checksum.as_slice() == migration.checksum.as_ref() {
            continue;
        }

        let Some(alternate_checksum) = alternate_line_ending_checksum(migration) else {
            transaction.rollback().await?;
            return Ok(0);
        };
        if stored_checksum != alternate_checksum {
            transaction.rollback().await?;
            return Ok(0);
        }

        repairs.push((version, stored_checksum, migration.checksum.to_vec()));
    }

    for (version, old_checksum, current_checksum) in &repairs {
        let result = sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = ? WHERE version = ? AND checksum = ? AND success = 1",
        )
        .bind(current_checksum)
        .bind(version)
        .bind(old_checksum)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!(
                "migration checksum changed while applying line-ending compatibility repair"
            );
        }
    }

    transaction.commit().await?;
    Ok(repairs.len())
}

fn alternate_line_ending_checksum(migration: &Migration) -> Option<Vec<u8>> {
    let sql = migration.sql.as_str();
    let bytes = sql.as_bytes();
    let mut has_crlf = false;
    let mut has_lf = false;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                has_crlf = true;
                index += 2;
            }
            b'\r' => return None,
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    if has_crlf == has_lf {
        return None;
    }

    let alternate_sql = if has_crlf {
        sql.replace("\r\n", "\n")
    } else {
        sql.replace('\n', "\r\n")
    };
    let alternate = Migration::new(
        migration.version,
        migration.description.clone(),
        migration.migration_type,
        AssertSqlSafe(alternate_sql).into_sql_str(),
        migration.no_tx,
    );
    Some(alternate.checksum.into_owned())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
