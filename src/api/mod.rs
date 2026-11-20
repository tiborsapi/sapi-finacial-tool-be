use axum::{routing::get, Router};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;

pub mod expenses;
pub mod health;
pub mod incomes;
mod records;

#[cfg(test)]
mod tests;

/// The full HTTP application. Shared by `main` and the API integration tests.
pub fn app(pool: PgPool) -> Router {
    Router::new()
        .merge(incomes::router())
        .merge(expenses::router())
        .route("/health", get(health::health_check))
        .layer(TraceLayer::new_for_http())
        .with_state(pool)
}
