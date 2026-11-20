use anyhow::Result;
use sqlx::{Pool, Postgres};

pub async fn connect(database_url: &str) -> Result<Pool<Postgres>> {
    let pool = sqlx::PgPool::connect(database_url).await?;
    Ok(pool)
}

pub async fn migrate(pool: &Pool<Postgres>) -> Result<()> {
    // Idempotent scripts (IF NOT EXISTS), applied in order on every startup.
    for script in [
        include_str!("../../migrations/0001_create_incomes.sql"),
        include_str!("../../migrations/0002_create_expenses.sql"),
    ] {
        sqlx::raw_sql(script).execute(pool).await?;
    }
    Ok(())
}
