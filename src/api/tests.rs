//! HTTP-level integration tests: the real router against a real PostgreSQL database.
//!
//! Each test gets its own throwaway database from `#[sqlx::test]`, so tests are
//! isolated and can run in parallel. They need `DATABASE_URL` (a superuser or a
//! role allowed to create databases), so they are `#[ignore]`d for a plain
//! `cargo test`. Run them with:
//!
//! ```text
//! DATABASE_URL=postgres://finance:finance_dev_password@localhost:5433/finance_db \
//!     cargo test -- --include-ignored
//! ```
//!
//! Every scenario runs once for incomes and once for expenses.

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

/// What differs between the two kinds as seen from outside the API.
#[derive(Clone, Copy)]
struct Spec {
    path: &'static str,
    other_path: &'static str,
    category: &'static str,
    other_category: &'static str,
    /// A category that is valid for the *other* kind only.
    foreign_category: &'static str,
}

const INCOME: Spec = Spec {
    path: "/api/incomes",
    other_path: "/api/expenses",
    category: "Salary",
    other_category: "Freelance",
    foreign_category: "Rent",
};

const EXPENSE: Spec = Spec {
    path: "/api/expenses",
    other_path: "/api/incomes",
    category: "Rent",
    other_category: "Food",
    foreign_category: "Salary",
};

async fn app(pool: PgPool) -> Router {
    crate::db::migrate(&pool).await.unwrap();
    crate::api::app(pool)
}

async fn call(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    call(app, Method::GET, uri, None).await
}

fn body(spec: Spec) -> Value {
    json!({
        "amount_cents": 12_50,
        "source": "Acme",
        "category": spec.category,
        "description": "March",
        "received_on": "2026-03-15"
    })
}

async fn create(app: &Router, spec: Spec, payload: Value) -> Value {
    let (status, value) = call(app, Method::POST, spec.path, Some(payload)).await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    value
}

async fn total_items(app: &Router, path: &str) -> i64 {
    get(app, path).await.1["pagination"]["total_items"]
        .as_i64()
        .unwrap()
}

/// Twelve records, amounts 100..=1200, alternating categories, one per day in January 2026.
/// Inserted in a shuffled order so creation order differs from date, amount and source order.
async fn seed(app: &Router, spec: Spec) {
    for step in 0..12 {
        let i = (step * 5) % 12; // 5 is coprime with 12: visits each i once
        let category = if i % 2 == 0 {
            spec.category
        } else {
            spec.other_category
        };
        create(
            app,
            spec,
            json!({
                "amount_cents": (i + 1) * 100,
                "source": format!("Source {i:02}"),
                "category": category,
                "description": if i % 3 == 0 { json!(format!("Note {i}")) } else { Value::Null },
                "received_on": format!("2026-01-{:02}", i + 1),
            }),
        )
        .await;
    }
}

fn ids(list: &Value) -> Vec<String> {
    list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

fn amounts(list: &Value) -> Vec<i64> {
    list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["amount_cents"].as_i64().unwrap())
        .collect()
}

/// Defines `<name>::{incomes, expenses}` tests that run the same body for each kind.
macro_rules! api_test {
    ($name:ident) => {
        mod $name {
            use super::*;

            #[sqlx::test(migrations = false)]
            #[ignore = "needs DATABASE_URL"]
            async fn incomes(pool: PgPool) {
                super::$name(app(pool).await, INCOME).await;
            }

            #[sqlx::test(migrations = false)]
            #[ignore = "needs DATABASE_URL"]
            async fn expenses(pool: PgPool) {
                super::$name(app(pool).await, EXPENSE).await;
            }
        }
    };
}

// ---------- create / read ----------

async fn create_returns_201_and_can_be_read_back(app: Router, spec: Spec) {
    let created = create(&app, spec, body(spec)).await;
    assert_eq!(created["amount_cents"], 1250);
    assert_eq!(created["source"], "Acme");
    assert_eq!(created["category"], spec.category);
    assert_eq!(created["description"], "March");
    assert_eq!(created["received_on"], "2026-03-15");
    assert!(created["created_at"].is_string());

    let id = created["id"].as_str().unwrap();
    let (status, fetched) = get(&app, &format!("{}/{id}", spec.path)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, created);
}
api_test!(create_returns_201_and_can_be_read_back);

async fn create_normalizes_input(app: Router, spec: Spec) {
    let created = create(
        &app,
        spec,
        json!({
            "amount_cents": 500,
            "source": "  Padded  ",
            "category": format!(" {} ", spec.category),
            "description": "   ",
            "received_on": "2026-03-15"
        }),
    )
    .await;
    assert_eq!(created["source"], "Padded");
    assert_eq!(created["category"], spec.category);
    assert_eq!(created["description"], Value::Null);
}
api_test!(create_normalizes_input);

async fn create_rejects_invalid_input_and_stores_nothing(app: Router, spec: Spec) {
    let with = |field: &str, value: Value| {
        let mut payload = body(spec);
        payload[field] = value;
        payload
    };
    let invalid = [
        with("amount_cents", json!(0)),
        with("amount_cents", json!(-5)),
        with("source", json!("   ")),
        with("source", json!("s".repeat(121))),
        with("category", json!("Nope")),
        with("category", json!(spec.foreign_category)),
        with("description", json!("d".repeat(501))),
    ];
    for payload in invalid {
        let (status, error) = call(&app, Method::POST, spec.path, Some(payload.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{payload}");
        assert!(error["error"].is_string());
    }
    assert_eq!(total_items(&app, spec.path).await, 0);
}
api_test!(create_rejects_invalid_input_and_stores_nothing);

async fn create_requires_an_integer_amount(app: Router, spec: Spec) {
    // Amounts are integer cents; a missing or fractional amount must never be guessed at.
    let mut missing_amount = body(spec);
    missing_amount
        .as_object_mut()
        .unwrap()
        .remove("amount_cents");
    let mut float_amount = body(spec);
    float_amount["amount_cents"] = json!(12.5);
    for payload in [missing_amount, float_amount] {
        let (status, _) = call(&app, Method::POST, spec.path, Some(payload.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{payload}");
    }
    assert_eq!(total_items(&app, spec.path).await, 0);
}
api_test!(create_requires_an_integer_amount);

/// The columns are VARCHAR(n) characters; a limit counted in bytes would reject this.
async fn create_accepts_multibyte_text_at_the_character_limit(app: Router, spec: Spec) {
    let mut payload = body(spec);
    payload["source"] = json!("é".repeat(120));
    payload["description"] = json!("ü".repeat(500));
    let created = create(&app, spec, payload).await;
    assert_eq!(created["source"].as_str().unwrap().chars().count(), 120);
    assert_eq!(
        created["description"].as_str().unwrap().chars().count(),
        500
    );
}
api_test!(create_accepts_multibyte_text_at_the_character_limit);

async fn get_unknown_id_returns_404(app: Router, spec: Spec) {
    let unknown = uuid::Uuid::new_v4();
    let (status, error) = get(&app, &format!("{}/{unknown}", spec.path)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(error["error"].as_str().unwrap().contains("not found"));
}
api_test!(get_unknown_id_returns_404);

// ---------- update ----------

async fn update_changes_fields_and_bumps_updated_at(app: Router, spec: Spec) {
    let created = create(&app, spec, body(spec)).await;
    let id = created["id"].as_str().unwrap();

    let mut change = body(spec);
    change["amount_cents"] = json!(9_999);
    change["source"] = json!("Renamed");
    change["category"] = json!(spec.other_category);
    change["description"] = Value::Null;
    change["received_on"] = json!("2026-04-01");
    let (status, updated) = call(
        &app,
        Method::PUT,
        &format!("{}/{id}", spec.path),
        Some(change),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["amount_cents"], 9999);
    assert_eq!(updated["source"], "Renamed");
    assert_eq!(updated["category"], spec.other_category);
    assert_eq!(updated["description"], Value::Null);
    assert_eq!(updated["received_on"], "2026-04-01");
    assert_eq!(updated["created_at"], created["created_at"]);
    assert_ne!(updated["updated_at"], created["updated_at"]);

    let (_, fetched) = get(&app, &format!("{}/{id}", spec.path)).await;
    assert_eq!(fetched, updated);

    // An invalid update is rejected and leaves the stored record as it was.
    let mut bad = body(spec);
    bad["amount_cents"] = json!(0);
    let (status, _) = call(&app, Method::PUT, &format!("{}/{id}", spec.path), Some(bad)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, after_bad_update) = get(&app, &format!("{}/{id}", spec.path)).await;
    assert_eq!(after_bad_update, updated);
}
api_test!(update_changes_fields_and_bumps_updated_at);

async fn update_unknown_id_returns_404(app: Router, spec: Spec) {
    let unknown = uuid::Uuid::new_v4();
    let (status, _) = call(
        &app,
        Method::PUT,
        &format!("{}/{unknown}", spec.path),
        Some(body(spec)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
api_test!(update_unknown_id_returns_404);

// ---------- delete ----------

async fn delete_removes_the_record(app: Router, spec: Spec) {
    let created = create(&app, spec, body(spec)).await;
    let keep = create(&app, spec, body(spec)).await;
    let id = created["id"].as_str().unwrap();

    let (status, _) = call(&app, Method::DELETE, &format!("{}/{id}", spec.path), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&app, &format!("{}/{id}", spec.path)).await.0,
        StatusCode::NOT_FOUND
    );

    // Deleting again is a 404, and the other record is untouched.
    let (status, _) = call(&app, Method::DELETE, &format!("{}/{id}", spec.path), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        ids(&get(&app, spec.path).await.1),
        vec![keep["id"].as_str().unwrap()]
    );
}
api_test!(delete_removes_the_record);

// ---------- list ----------

async fn empty_list_has_zero_totals(app: Router, spec: Spec) {
    let (status, list) = get(&app, spec.path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"], json!([]));
    assert_eq!(list["total_amount_cents"], 0);
    assert_eq!(list["pagination"]["total_items"], 0);
    assert_eq!(list["pagination"]["total_pages"], 0);
    assert_eq!(list["pagination"]["page"], 1);
    assert_eq!(list["pagination"]["per_page"], 10);
}
api_test!(empty_list_has_zero_totals);

async fn list_paginates_and_totals_cover_all_pages(app: Router, spec: Spec) {
    seed(&app, spec).await;
    let total: i64 = (1..=12).map(|n| n * 100).sum();

    let (_, page1) = get(&app, &format!("{}?per_page=5&page=1", spec.path)).await;
    let (_, page2) = get(&app, &format!("{}?per_page=5&page=2", spec.path)).await;
    let (_, page3) = get(&app, &format!("{}?per_page=5&page=3", spec.path)).await;
    let (_, page4) = get(&app, &format!("{}?per_page=5&page=4", spec.path)).await;

    assert_eq!(page1["pagination"]["total_items"], 12);
    assert_eq!(page1["pagination"]["total_pages"], 3);
    assert_eq!(page1["total_amount_cents"], total, "total spans all pages");
    assert_eq!(page3["total_amount_cents"], total);
    assert_eq!(ids(&page1).len(), 5);
    assert_eq!(ids(&page2).len(), 5);
    assert_eq!(ids(&page3).len(), 2);
    assert!(
        ids(&page4).is_empty(),
        "past the last page is empty, not an error"
    );

    // No row is duplicated or lost across pages.
    let mut all: Vec<String> = [&page1, &page2, &page3]
        .iter()
        .flat_map(|p| ids(p))
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 12);
}
api_test!(list_paginates_and_totals_cover_all_pages);

/// Seeded rows rise together in amount, date and source, but were created in a shuffled
/// order, and they alternate between two categories, so each sort field has a known order.
async fn list_sorts_by_each_allowed_field(app: Router, spec: Spec) {
    seed(&app, spec).await;
    let ascending: Vec<i64> = (1..=12).map(|n| n * 100).collect();
    let descending: Vec<i64> = ascending.iter().rev().copied().collect();
    let sorted = |field: &'static str, order: &'static str| {
        let uri = format!(
            "{}?per_page=100&sort_by={field}&sort_order={order}",
            spec.path
        );
        let app = app.clone();
        async move { get(&app, &uri).await.1 }
    };

    // Default is newest received_on first.
    let (_, default) = get(&app, &format!("{}?per_page=100", spec.path)).await;
    assert_eq!(amounts(&default), descending);

    for field in ["received_on", "amount", "source"] {
        assert_eq!(
            amounts(&sorted(field, "asc").await),
            ascending,
            "{field} asc"
        );
        assert_eq!(
            amounts(&sorted(field, "desc").await),
            descending,
            "{field} desc"
        );
    }

    // Creation order is the seed's insertion sequence, not the amount order.
    let created: Vec<i64> = (0..12).map(|step| ((step * 5) % 12 + 1) * 100).collect();
    let created_reversed: Vec<i64> = created.iter().rev().copied().collect();
    assert_eq!(amounts(&sorted("created_at", "asc").await), created);
    assert_eq!(
        amounts(&sorted("created_at", "desc").await),
        created_reversed
    );

    // Category has only two values, so check it is grouped and ordered both ways.
    let categories = |list: &Value| -> Vec<String> {
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["category"].as_str().unwrap().to_owned())
            .collect()
    };
    let by_category_asc = categories(&sorted("category", "asc").await);
    let mut expected = by_category_asc.clone();
    expected.sort();
    assert_eq!(by_category_asc, expected);
    assert_ne!(by_category_asc.first(), by_category_asc.last());
    let mut by_category_desc = categories(&sorted("category", "desc").await);
    by_category_desc.reverse();
    assert_eq!(by_category_desc, expected);
}
api_test!(list_sorts_by_each_allowed_field);

async fn sorting_on_ties_falls_back_to_id_order(app: Router, spec: Spec) {
    // 25 identical rows tie on every sort column, so only the `id ASC` tie-break
    // decides the order. It must be deterministic and consistent across pages.
    for _ in 0..25 {
        create(&app, spec, body(spec)).await;
    }
    let mut seen = Vec::new();
    for page in 1..=5 {
        let uri = format!("{}?per_page=5&page={page}&sort_by=category", spec.path);
        seen.extend(ids(&get(&app, &uri).await.1));
    }
    let mut expected = seen.clone();
    expected.sort();
    expected.dedup();
    assert_eq!(
        expected.len(),
        25,
        "a row appeared on two pages or went missing"
    );
    assert_eq!(seen, expected, "ties are not ordered by id");
}
api_test!(sorting_on_ties_falls_back_to_id_order);

async fn list_filters_by_search_category_and_dates(app: Router, spec: Spec) {
    seed(&app, spec).await;

    // Search matches source and description, case-insensitively.
    let (_, by_source) = get(&app, &format!("{}?q=source%2005", spec.path)).await;
    assert_eq!(amounts(&by_source), vec![600]);
    let (_, by_description) = get(&app, &format!("{}?q=NOTE%203", spec.path)).await;
    assert_eq!(amounts(&by_description), vec![400]);
    let (_, blank) = get(&app, &format!("{}?q=%20%20", spec.path)).await;
    assert_eq!(
        blank["pagination"]["total_items"], 12,
        "blank search is ignored"
    );
    let (_, none) = get(&app, &format!("{}?q=zzz", spec.path)).await;
    assert_eq!(none["pagination"]["total_items"], 0);
    assert_eq!(none["total_amount_cents"], 0);

    // Category.
    let (_, by_category) = get(&app, &format!("{}?category={}", spec.path, spec.category)).await;
    assert_eq!(by_category["pagination"]["total_items"], 6);
    assert_eq!(
        by_category["total_amount_cents"],
        100 + 300 + 500 + 700 + 900 + 1100
    );

    // Inclusive date range: Jan 3 to Jan 5 is amounts 300, 400, 500.
    let (_, ranged) = get(
        &app,
        &format!(
            "{}?from=2026-01-03&to=2026-01-05&sort_by=amount&sort_order=asc",
            spec.path
        ),
    )
    .await;
    assert_eq!(amounts(&ranged), vec![300, 400, 500]);
    let (_, from_only) = get(&app, &format!("{}?from=2026-01-11", spec.path)).await;
    assert_eq!(from_only["pagination"]["total_items"], 2);
    let (_, to_only) = get(&app, &format!("{}?to=2026-01-02", spec.path)).await;
    assert_eq!(to_only["pagination"]["total_items"], 2);

    // Filters combine, and totals follow the filter.
    let (_, combined) = get(
        &app,
        &format!(
            "{}?category={}&from=2026-01-01&to=2026-01-04",
            spec.path, spec.category
        ),
    )
    .await;
    assert_eq!(combined["pagination"]["total_items"], 2);
    assert_eq!(combined["total_amount_cents"], 100 + 300);
}
api_test!(list_filters_by_search_category_and_dates);

async fn list_rejects_invalid_query_parameters(app: Router, spec: Spec) {
    for query in [
        "page=0",
        "per_page=0",
        "per_page=101",
        "sort_by=id",
        "sort_by=amount_cents",
        "sort_order=sideways",
        "category=Nope",
        &format!("q={}", "q".repeat(121)),
        "from=2026-02-01&to=2026-01-01",
    ] {
        let (status, _) = get(&app, &format!("{}?{query}", spec.path)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
    }
    let foreign = format!("{}?category={}", spec.path, spec.foreign_category);
    assert_eq!(
        get(&app, &foreign).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
api_test!(list_rejects_invalid_query_parameters);

async fn hostile_input_is_treated_as_data(app: Router, spec: Spec) {
    seed(&app, spec).await;
    let attack = "x'%3B%20DROP%20TABLE%20incomes%3B%20DROP%20TABLE%20expenses%3B--";
    let (status, list) = get(&app, &format!("{}?q={attack}", spec.path)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["pagination"]["total_items"], 0);

    let mut payload = body(spec);
    payload["source"] = json!("Robert'); DROP TABLE incomes;--");
    let created = create(&app, spec, payload).await;
    assert_eq!(created["source"], "Robert'); DROP TABLE incomes;--");

    // Both tables still exist and hold data.
    assert_eq!(total_items(&app, spec.path).await, 13);
    assert_eq!(get(&app, spec.other_path).await.0, StatusCode::OK);
}
api_test!(hostile_input_is_treated_as_data);

// ---------- kinds stay separate ----------

async fn kinds_do_not_share_data(app: Router, spec: Spec) {
    let created = create(&app, spec, body(spec)).await;
    let id = created["id"].as_str().unwrap();

    assert_eq!(total_items(&app, spec.other_path).await, 0);
    assert_eq!(
        get(&app, &format!("{}/{id}", spec.other_path)).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(
        &app,
        Method::DELETE,
        &format!("{}/{id}", spec.other_path),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        total_items(&app, spec.path).await,
        1,
        "other kind's delete had no effect"
    );
}
api_test!(kinds_do_not_share_data);

// ---------- app-level ----------

#[sqlx::test(migrations = false)]
#[ignore = "needs DATABASE_URL"]
async fn health_check_reports_ok(pool: PgPool) {
    let (status, value) = get(&app(pool).await, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["status"], "ok");
}

#[sqlx::test(migrations = false)]
#[ignore = "needs DATABASE_URL"]
async fn migrations_are_idempotent_and_keep_data(pool: PgPool) {
    let app = app(pool.clone()).await;
    create(&app, INCOME, body(INCOME)).await;
    create(&app, EXPENSE, body(EXPENSE)).await;

    crate::db::migrate(&pool).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();

    assert_eq!(total_items(&app, INCOME.path).await, 1);
    assert_eq!(total_items(&app, EXPENSE.path).await, 1);
}
