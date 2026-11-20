use axum::Router;
use sqlx::PgPool;

use super::records::{self, Kind};

pub struct Expense;

impl Kind for Expense {
    const TABLE: &'static str = "expenses";
    const NOUN: &'static str = "expense";
    const LABEL: &'static str = "Expense";
    const PATH: &'static str = "/api/expenses";
    const CATEGORIES: &'static [&'static str] =
        &["Rent", "Food", "Transport", "Utilities", "Health", "Other"];
}

pub fn router() -> Router<PgPool> {
    records::router::<Expense>()
}
