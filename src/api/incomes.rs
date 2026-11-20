use axum::Router;
use sqlx::PgPool;

use super::records::{self, Kind};

pub struct Income;

impl Kind for Income {
    const TABLE: &'static str = "incomes";
    const NOUN: &'static str = "income";
    const LABEL: &'static str = "Income";
    const PATH: &'static str = "/api/incomes";
    const CATEGORIES: &'static [&'static str] =
        &["Salary", "Freelance", "Investments", "Business", "Other"];
}

pub fn router() -> Router<PgPool> {
    records::router::<Income>()
}
