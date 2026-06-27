use crate::auth::AppState;
use crate::escape::html_escape;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;

pub async fn public_menu(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    // Only serve published restaurants; 404 for drafts (safe default).
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

    // Single query: categories with at least one available item (INNER JOIN).
    // Rows are ordered so all items for a category are consecutive.
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

    // Group rows into (category_name, items) sections in order.
    let mut sections: Vec<(String, Vec<(String, Option<String>, i32)>)> = Vec::new();
    let mut current_cat: Option<i64> = None;

    for (cat_id, cat_name, item_name, item_desc, price_cents) in rows {
        if current_cat != Some(cat_id) {
            sections.push((cat_name, Vec::new()));
            current_cat = Some(cat_id);
        }
        if let Some(sec) = sections.last_mut() {
            sec.1.push((item_name, item_desc, price_cents));
        }
    }

    let body = if sections.is_empty() {
        "<p>No items available yet.</p>".to_string()
    } else {
        sections
            .iter()
            .map(|(cat_name, items)| {
                let cat_e = html_escape(cat_name);
                let items_html = items
                    .iter()
                    .map(|(name, desc, price_cents)| {
                        let name_e = html_escape(name);
                        let price = format!("{:.2}", *price_cents as f64 / 100.0);
                        let desc_html = desc
                            .as_deref()
                            .filter(|d| !d.is_empty())
                            .map(|d| format!("<p class=\"desc\">{}</p>", html_escape(d)))
                            .unwrap_or_default();
                        format!(
                            "<li><span class=\"name\">{name_e}</span>\
                             <span class=\"price\">€{price}</span>{desc_html}</li>"
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("<h2>{cat_e}</h2>\n<ul class=\"items\">\n{items_html}\n</ul>")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let name_e = html_escape(&restaurant_name);
    Ok(Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{name_e}</title>
<style>
body{{font-family:sans-serif;max-width:600px;margin:0 auto;padding:1rem}}
h1{{font-size:1.8rem;margin-bottom:.5rem}}
h2{{font-size:1.2rem;margin-top:1.5rem;border-bottom:1px solid #ccc;padding-bottom:.2rem}}
ul.items{{list-style:none;padding:0;margin:0}}
ul.items li{{display:flex;flex-wrap:wrap;justify-content:space-between;
              align-items:baseline;padding:.5rem 0;border-bottom:1px solid #eee}}
.name{{font-weight:bold;flex:1}}
.price{{color:#555;white-space:nowrap;margin-left:1rem}}
p.desc{{width:100%;margin:.2rem 0 0;font-size:.9rem;color:#666}}
</style>
</head>
<body>
<h1>{name_e}</h1>
{body}
</body>
</html>"#
    )))
}
