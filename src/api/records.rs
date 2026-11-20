//! Shared CRUD, validation and listing logic for money records.
//!
//! Incomes and expenses have the same shape and rules; only the table,
//! labels, URL and category list differ. Those differences live in [`Kind`].

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

const DEFAULT_PAGE_SIZE: u32 = 10;
const MAX_PAGE_SIZE: u32 = 100;

/// What differs between record kinds (incomes, expenses). Everything else is shared.
pub trait Kind: Send + Sync + 'static {
    /// Database table. Interpolated into SQL, so it must be a trusted constant.
    const TABLE: &'static str;
    /// Lowercase singular name used in messages and audit events ("income").
    const NOUN: &'static str;
    /// Capitalised singular name used in messages ("Income").
    const LABEL: &'static str;
    /// URL prefix, e.g. "/api/incomes".
    const PATH: &'static str;
    const CATEGORIES: &'static [&'static str];
}

pub fn router<K: Kind>() -> Router<PgPool> {
    Router::new()
        .route(K::PATH, get(list_records::<K>).post(create_record::<K>))
        .route(
            &format!("{}/:id", K::PATH),
            get(get_record::<K>)
                .put(update_record::<K>)
                .delete(delete_record::<K>),
        )
}

#[derive(Debug, Serialize, FromRow)]
pub struct Record {
    id: Uuid,
    amount_cents: i64,
    source: String,
    category: String,
    description: Option<String>,
    received_on: NaiveDate,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct RecordInput {
    amount_cents: i64,
    source: String,
    category: String,
    description: Option<String>,
    received_on: NaiveDate,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Filters {
    page: u32,
    per_page: u32,
    q: Option<String>,
    category: Option<String>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    sort_by: String,
    sort_order: String,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: DEFAULT_PAGE_SIZE,
            q: None,
            category: None,
            from: None,
            to: None,
            sort_by: "received_on".to_owned(),
            sort_order: "desc".to_owned(),
        }
    }
}

#[derive(Serialize)]
struct RecordList {
    data: Vec<Record>,
    pagination: Pagination,
    total_amount_cents: i64,
}

#[derive(Serialize)]
struct Pagination {
    page: u32,
    per_page: u32,
    total_items: i64,
    total_pages: i64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    fn not_found<K: Kind>() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("{} record not found", K::LABEL),
        }
    }

    fn internal<K: Kind>(error: sqlx::Error) -> Self {
        tracing::error!(%error, "{} request failed", K::LABEL);
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("The {} request could not be completed", K::NOUN),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

fn validate_input<K: Kind>(mut input: RecordInput) -> Result<RecordInput, ApiError> {
    input.source = input.source.trim().to_owned();
    input.category = input.category.trim().to_owned();
    input.description = input
        .description
        .map(|description| description.trim().to_owned())
        .filter(|description| !description.is_empty());

    if input.amount_cents <= 0 {
        return Err(ApiError::bad_request("Amount must be greater than zero"));
    }
    if input.source.is_empty() || input.source.chars().count() > 120 {
        return Err(ApiError::bad_request(
            "Source must contain between 1 and 120 characters",
        ));
    }
    if !K::CATEGORIES.contains(&input.category.as_str()) {
        return Err(invalid_category::<K>());
    }
    if input
        .description
        .as_ref()
        .is_some_and(|description| description.chars().count() > 500)
    {
        return Err(ApiError::bad_request(
            "Description must be at most 500 characters",
        ));
    }

    Ok(input)
}

fn invalid_category<K: Kind>() -> ApiError {
    ApiError::bad_request(format!("Choose a valid {} category", K::NOUN))
}

fn validate_filters<K: Kind>(filters: &Filters) -> Result<(), ApiError> {
    if filters.page == 0 {
        return Err(ApiError::bad_request("Page must be at least 1"));
    }
    if filters.per_page == 0 || filters.per_page > MAX_PAGE_SIZE {
        return Err(ApiError::bad_request("Page size must be between 1 and 100"));
    }
    if filters
        .category
        .as_ref()
        .is_some_and(|category| !K::CATEGORIES.contains(&category.as_str()))
    {
        return Err(invalid_category::<K>());
    }
    if filters
        .q
        .as_deref()
        .is_some_and(|search| search.chars().count() > 120)
    {
        return Err(ApiError::bad_request(
            "Search must be at most 120 characters",
        ));
    }
    if filters
        .from
        .zip(filters.to)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(ApiError::bad_request(
            "The start date must be on or before the end date",
        ));
    }
    Ok(())
}

fn sort_column(sort_by: &str) -> Result<&'static str, ApiError> {
    match sort_by {
        "received_on" => Ok("received_on"),
        "amount" => Ok("amount_cents"),
        "source" => Ok("source"),
        "category" => Ok("category"),
        "created_at" => Ok("created_at"),
        _ => Err(ApiError::bad_request(
            "Sort by received_on, amount, source, category, or created_at",
        )),
    }
}

fn sort_direction(sort_order: &str) -> Result<&'static str, ApiError> {
    match sort_order {
        "asc" => Ok("ASC"),
        "desc" => Ok("DESC"),
        _ => Err(ApiError::bad_request("Sort order must be asc or desc")),
    }
}

const COLUMNS: &str =
    "id, amount_cents, source, category, description, received_on, created_at, updated_at";

async fn list_records<K: Kind>(
    State(pool): State<PgPool>,
    Query(filters): Query<Filters>,
) -> Result<Json<RecordList>, ApiError> {
    validate_filters::<K>(&filters)?;
    let sort_column = sort_column(&filters.sort_by)?;
    let sort_direction = sort_direction(&filters.sort_order)?;
    let category = filters.category.as_deref();
    let from = filters.from;
    let to = filters.to;
    let search = filters
        .q
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(|search| format!("%{search}%"));
    let page_size = i64::from(filters.per_page);
    let offset = i64::from(filters.page - 1) * page_size;
    let table = K::TABLE;
    let where_clause = "
        ($1::text IS NULL OR category = $1)
        AND ($2::date IS NULL OR received_on >= $2)
        AND ($3::date IS NULL OR received_on <= $3)
        AND ($4::text IS NULL OR source ILIKE $4 OR COALESCE(description, '') ILIKE $4)
    ";

    let (total_items, total_amount_cents): (i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COALESCE(SUM(amount_cents), 0)::BIGINT FROM {table} WHERE {where_clause}"
    ))
    .bind(category)
    .bind(from)
    .bind(to)
    .bind(&search)
    .fetch_one(&pool)
    .await
    .map_err(ApiError::internal::<K>)?;

    let data = sqlx::query_as::<_, Record>(&format!(
        "SELECT {COLUMNS} FROM {table} WHERE {where_clause}
         ORDER BY {sort_column} {sort_direction}, id ASC
         LIMIT $5 OFFSET $6"
    ))
    .bind(category)
    .bind(from)
    .bind(to)
    .bind(search)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(ApiError::internal::<K>)?;

    Ok(Json(RecordList {
        data,
        pagination: Pagination {
            page: filters.page,
            per_page: filters.per_page,
            total_items,
            total_pages: (total_items + page_size - 1) / page_size,
        },
        total_amount_cents,
    }))
}

async fn get_record<K: Kind>(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Record>, ApiError> {
    let table = K::TABLE;
    sqlx::query_as::<_, Record>(&format!("SELECT {COLUMNS} FROM {table} WHERE id = $1"))
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(ApiError::internal::<K>)?
        .map(Json)
        .ok_or_else(ApiError::not_found::<K>)
}

async fn create_record<K: Kind>(
    State(pool): State<PgPool>,
    Json(input): Json<RecordInput>,
) -> Result<(StatusCode, Json<Record>), ApiError> {
    let input = validate_input::<K>(input)?;
    let table = K::TABLE;
    let record = sqlx::query_as::<_, Record>(&format!(
        "INSERT INTO {table} (id, amount_cents, source, category, description, received_on)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(input.amount_cents)
    .bind(input.source)
    .bind(input.category)
    .bind(input.description)
    .bind(input.received_on)
    .fetch_one(&pool)
    .await
    .map_err(ApiError::internal::<K>)?;

    tracing::info!(
        record_id = %record.id,
        amount_cents = record.amount_cents,
        category = %record.category,
        "{}_created",
        K::NOUN
    );
    Ok((StatusCode::CREATED, Json(record)))
}

async fn update_record<K: Kind>(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<RecordInput>,
) -> Result<Json<Record>, ApiError> {
    let input = validate_input::<K>(input)?;
    let table = K::TABLE;
    let record = sqlx::query_as::<_, Record>(&format!(
        "UPDATE {table}
         SET amount_cents = $2, source = $3, category = $4, description = $5,
             received_on = $6, updated_at = NOW()
         WHERE id = $1
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(input.amount_cents)
    .bind(input.source)
    .bind(input.category)
    .bind(input.description)
    .bind(input.received_on)
    .fetch_optional(&pool)
    .await
    .map_err(ApiError::internal::<K>)?
    .ok_or_else(ApiError::not_found::<K>)?;

    tracing::info!(
        record_id = %record.id,
        amount_cents = record.amount_cents,
        category = %record.category,
        "{}_updated",
        K::NOUN
    );
    Ok(Json(record))
}

async fn delete_record<K: Kind>(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let table = K::TABLE;
    let result = sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
        .bind(id)
        .execute(&pool)
        .await
        .map_err(ApiError::internal::<K>)?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found::<K>());
    }
    tracing::info!(record_id = %id, "{}_deleted", K::NOUN);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    //! Unit tests for rules the HTTP tests in `api/tests.rs` cannot reach cheaply:
    //! input normalisation, length boundaries, and the sort/filter whitelists.
    //! Everything observable over HTTP is tested there instead.
    use super::*;
    use crate::api::{expenses::Expense, incomes::Income};

    fn valid_category<K: Kind>() -> &'static str {
        K::CATEGORIES[0]
    }

    fn valid_input<K: Kind>() -> RecordInput {
        RecordInput {
            amount_cents: 1250,
            source: "Consulting".to_owned(),
            category: valid_category::<K>().to_owned(),
            description: Some("Client work".to_owned()),
            received_on: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        }
    }

    // Each generic test body runs for both kinds so incomes and expenses can't drift apart.
    macro_rules! for_each_kind {
        ($name:ident, $body:ident) => {
            #[test]
            fn $name() {
                $body::<Income>();
                $body::<Expense>();
            }
        };
    }

    fn normalizes_valid_fields<K: Kind>() {
        let mut valid = valid_input::<K>();
        valid.source = "  Consulting  ".to_owned();
        valid.category = format!(" {} ", valid_category::<K>());
        valid.description = Some("  Client work  ".to_owned());

        let normalized = validate_input::<K>(valid).unwrap();
        assert_eq!(normalized.source, "Consulting");
        assert_eq!(normalized.category, valid_category::<K>());
        assert_eq!(normalized.description.as_deref(), Some("Client work"));
        assert_eq!(normalized.amount_cents, 1250);
    }
    for_each_kind!(normalizes_valid_input_fields, normalizes_valid_fields);

    fn blank_or_missing_description_becomes_none<K: Kind>() {
        for description in [Some("  \n ".to_owned()), None] {
            let mut input = valid_input::<K>();
            input.description = description;
            assert_eq!(validate_input::<K>(input).unwrap().description, None);
        }
    }
    for_each_kind!(
        converts_blank_or_missing_descriptions_to_none,
        blank_or_missing_description_becomes_none
    );

    /// The database limits are in characters (VARCHAR), so validation must count
    /// characters, not bytes: "é" is two bytes.
    fn length_limits_count_characters<K: Kind>() {
        let mut at_limit = valid_input::<K>();
        at_limit.source = "é".repeat(120);
        at_limit.description = Some("é".repeat(500));
        at_limit.amount_cents = 1;
        assert!(validate_input::<K>(at_limit).is_ok());

        let mut long_source = valid_input::<K>();
        long_source.source = "é".repeat(121);
        assert!(validate_input::<K>(long_source).is_err());

        let mut long_description = valid_input::<K>();
        long_description.description = Some("é".repeat(501));
        assert!(validate_input::<K>(long_description).is_err());
    }
    for_each_kind!(
        length_limits_are_in_characters_not_bytes,
        length_limits_count_characters
    );

    #[test]
    fn category_errors_name_the_kind() {
        let message = |error: ApiError| error.message;
        let bad = |category: &str| category.to_owned();
        let income = validate_input::<Income>(RecordInput {
            category: bad("Unknown"),
            ..valid_input::<Income>()
        })
        .unwrap_err();
        let expense = validate_input::<Expense>(RecordInput {
            category: bad("Unknown"),
            ..valid_input::<Expense>()
        })
        .unwrap_err();
        assert_eq!(message(income), "Choose a valid income category");
        assert_eq!(message(expense), "Choose a valid expense category");
    }

    #[test]
    fn rejects_untrusted_sort_fields_and_orders() {
        // These strings reach SQL text, so only a whitelist may pass.
        for invalid in ["id", "id; DROP TABLE expenses", "amount_cents", ""] {
            assert_eq!(
                sort_column(invalid).unwrap_err().status,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        for invalid in ["random", "DESC", "asc; DROP TABLE incomes", ""] {
            assert_eq!(
                sort_direction(invalid).unwrap_err().status,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    }

    fn accepts_filter_boundaries<K: Kind>() {
        for per_page in [1, MAX_PAGE_SIZE] {
            let filters = Filters {
                per_page,
                ..Filters::default()
            };
            assert!(validate_filters::<K>(&filters).is_ok());
        }
        let same_day = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let filters = Filters {
            from: Some(same_day),
            to: Some(same_day),
            q: Some("q".repeat(120)),
            category: Some(valid_category::<K>().to_owned()),
            ..Filters::default()
        };
        assert!(validate_filters::<K>(&filters).is_ok());
    }
    for_each_kind!(
        accepts_valid_listing_filters_and_boundaries,
        accepts_filter_boundaries
    );
}
