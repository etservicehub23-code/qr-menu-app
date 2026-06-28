use crate::auth::AppState;
use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;

struct MenuItem {
    name: String,
    description: Option<String>,
    price: String,
}

struct MenuSection {
    category_name: String,
    items: Vec<MenuItem>,
}

#[derive(Template)]
#[template(path = "menu.html")]
struct MenuPage {
    restaurant_name: String,
    sections: Vec<MenuSection>,
}

pub async fn public_menu(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let restaurant: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM restaurants WHERE slug = $1 AND is_published = true",
    )
    .bind(&slug)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let Some((restaurant_id, restaurant_name)) = restaurant else {
        return Err((StatusCode::NOT_FOUND, "menu not found"));
    };

    let rows: Vec<(i64, String, String, Option<String>, i32)> = sqlx::query_as(
        "SELECT mc.id, mc.name, mi.name, mi.description, mi.price_cents
         FROM menu_categories mc
         JOIN menu_items mi ON mi.category_id = mc.id AND mi.is_available = true
         WHERE mc.restaurant_id = $1
         ORDER BY mc.sort_order, mc.id, mi.sort_order, mi.id",
    )
    .bind(restaurant_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let mut sections: Vec<MenuSection> = Vec::new();
    let mut current_cat: Option<i64> = None;

    for (cat_id, cat_name, item_name, item_desc, price_cents) in rows {
        if current_cat != Some(cat_id) {
            sections.push(MenuSection {
                category_name: cat_name,
                items: Vec::new(),
            });
            current_cat = Some(cat_id);
        }
        if let Some(sec) = sections.last_mut() {
            sec.items.push(MenuItem {
                name: item_name,
                description: item_desc.filter(|d| !d.is_empty()),
                price: format!("{:.2}", price_cents as f64 / 100.0),
            });
        }
    }

    let page = MenuPage {
        restaurant_name,
        sections,
    };

    let html = page
        .render()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template render error"))?;

    Ok(Html(html))
}
