//! Which cached storefront pages an outbox event invalidates (A2, WP6 follow-up). The worker
//! turns the answer into an edge purge. Page models tag themselves (`product:<id>` on product
//! pages and every listing that shows the product, `shop` on everything), so frequent changes
//! (price, stock, product edits) purge by tag; rare structural ones purge the tenant.

use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purge {
    /// Every cached page of the tenant.
    Tenant,
    Tags(Vec<String>),
}

/// Event types that change what the storefront renders.
pub const EVENTS: &[&str] = &[
    "product.created",
    "product.updated",
    "product.deleted",
    "price.changed",
    "inventory.changed",
    "category.created",
    "category.updated",
    "category.moved",
    "category.deleted",
    "market.created",
    "market.updated",
    "price_list.updated",
    crate::content::PAGE_CHANGED_EVENT,
    crate::content::MENU_CHANGED_EVENT,
];

pub fn for_event(event_type: &str, payload: &Value) -> Option<Purge> {
    let product = || {
        payload
            .get("product_id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .map(|id| Purge::Tags(vec![format!("product:{id}")]))
    };
    match event_type {
        // A new product can appear in any listing, home and search: purge the tenant.
        "product.updated" | "product.deleted" | "price.changed" | "inventory.changed" => product(),
        t if EVENTS.contains(&t) => Some(Purge::Tenant),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_map_to_purges() {
        let id = Uuid::now_v7();
        let p = json!({ "product_id": id, "variant_id": Uuid::now_v7() });
        for t in ["product.updated", "price.changed", "inventory.changed"] {
            assert_eq!(
                for_event(t, &p),
                Some(Purge::Tags(vec![format!("product:{id}")])),
                "{t}"
            );
        }
        assert_eq!(for_event("product.created", &p), Some(Purge::Tenant));
        assert_eq!(for_event("category.moved", &json!({})), Some(Purge::Tenant));
        assert_eq!(
            for_event(crate::content::PAGE_CHANGED_EVENT, &json!({})),
            Some(Purge::Tenant)
        );
        assert_eq!(for_event("coupon.created", &json!({})), None);
        assert_eq!(for_event("price.changed", &json!({})), None);
    }
}
