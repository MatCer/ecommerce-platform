use super::pii_rotation::{addresses, contact, holds_a_capability, place_body};
use super::*;

impl Ctx {
    async fn receipt(&self, cart: &str) -> (StatusCode, String) {
        let (status, text, _) = self
            .edge
            .call(Call::get("/storefront/v1/checkout/order"))
            .header("x-cart-token", cart.to_owned())
            .send_text(&self.s)
            .await;
        (status, text)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn lost_response_after_commit_returns_existing_order(db: PgPool) {
    let c = setup(db, CheckoutMode::SameOrigin).await;
    let method = c.placeable().await;
    let shop_token = c.shop_cart().await;
    let k0 = c.started(&shop_token, None).await;
    let k1 = c.rotated(&k0, contact("jana@example.cz")).await;
    let k2 = c.rotated(&k1, addresses("Jana Nováková")).await;
    let view = c.ready(&k2, method).await;

    let (status, text, res) = c
        .place(&k2, "p-1", &place_body(&view, "Zvonit dvakrát"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert!(
        replacement(&res).is_some(),
        "the note replaced K2, and this answer never reaches the browser"
    );
    let placed: Value = serde_json::from_str(&text).unwrap();

    let (status, text) = c.receipt(&k2).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the replaced capability finds its order: {text}"
    );
    let receipt: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(receipt["number"], placed["number"]);
    assert_eq!(receipt["email"], "jana@example.cz");
    assert!(
        !text.contains("Zvonit") && !text.contains("Nováková"),
        "the receipt holds no note or address: {text}"
    );
    assert!(
        !holds_a_capability(&text),
        "nor a capability or an order link: {text}"
    );
}
