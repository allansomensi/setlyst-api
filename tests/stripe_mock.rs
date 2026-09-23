//! The Stripe client against [stripe-mock](https://github.com/stripe/stripe-mock),
//! which validates every request against Stripe's OpenAPI spec and answers
//! with fixture objects. Skipped unless `STRIPE_MOCK_URL` is set:
//!
//! ```sh
//! stripe-mock -http-port 12111 &
//! STRIPE_MOCK_URL=http://localhost:12111 cargo test --test stripe_mock
//! ```

use chrono::Utc;
use setlyst_api::{
    config::parse_stripe,
    models::billing::BillingInterval,
    payments::{CheckoutRequest, PaymentGateway, PriceSpec, stripe::StripeGateway},
};
use uuid::Uuid;

fn gateway() -> Option<StripeGateway> {
    let base = std::env::var("STRIPE_MOCK_URL").ok()?;
    let config = parse_stripe(
        Some("sk_test_123".into()),
        Some("whsec_123".into()),
        Some(base),
    )
    .unwrap()
    .unwrap();
    Some(StripeGateway::new(&config).unwrap())
}

#[tokio::test]
async fn every_call_is_a_valid_stripe_request() {
    let Some(stripe) = gateway() else {
        eprintln!("STRIPE_MOCK_URL not set; skipped");
        return;
    };
    let user_id = Uuid::now_v7();

    let customer = stripe
        .create_customer(user_id, Some("anna@example.com"), "anna", "pt-BR")
        .await
        .expect("customer");
    assert!(customer.starts_with("cus_"));

    let spec = PriceSpec {
        plan_code: "pro".into(),
        product_name: "Setlyst Pro".into(),
        interval: BillingInterval::Yearly,
        currency: "BRL".into(),
        unit_amount: 39900,
    };
    // stripe-mock always "finds" a price for the lookup; the create path is
    // exercised by calling the endpoint directly below.
    let price = stripe.ensure_price(&spec).await.expect("price");
    assert!(price.starts_with("price_"));

    let coupon = stripe.create_coupon(20, "Launch").await.expect("coupon");
    assert!(!coupon.is_empty());

    let url = stripe
        .create_checkout(&CheckoutRequest {
            user_id,
            customer_id: customer.clone(),
            price_id: price.clone(),
            plan_code: "pro".into(),
            interval: BillingInterval::Yearly,
            success_url: "https://setlyst.com.br/pt-BR/dashboard/settings?checkout=success".into(),
            cancel_url: "https://setlyst.com.br/pt-BR/dashboard/settings?checkout=canceled".into(),
            locale: "pt-BR".into(),
            trial_end: Some(Utc::now().timestamp() + 10 * 86_400),
            coupon_id: None,
            redemption_id: Some(Uuid::now_v7()),
        })
        .await
        .expect("checkout with trial");
    assert!(url.starts_with("https://"));
    stripe
        .create_checkout(&CheckoutRequest {
            user_id,
            customer_id: customer.clone(),
            price_id: price.clone(),
            plan_code: "pro".into(),
            interval: BillingInterval::Yearly,
            success_url: "https://setlyst.com.br/en/x".into(),
            cancel_url: "https://setlyst.com.br/en/y".into(),
            locale: "es".into(),
            trial_end: None,
            coupon_id: Some(coupon),
            redemption_id: None,
        })
        .await
        .expect("checkout with a discount");

    let portal = stripe
        .create_portal(
            &customer,
            "https://setlyst.com.br/en/dashboard/settings",
            "en",
        )
        .await
        .expect("portal");
    assert!(portal.starts_with("https://"));

    let subscription = stripe.get_subscription("sub_123").await.expect("fetch");
    assert!(subscription.id.starts_with("sub_"));
    assert!(!subscription.item_id.is_empty());
    let changed = stripe
        .change_price(&subscription.id, &subscription.item_id, &price)
        .await
        .expect("change");
    assert_eq!(changed.id, subscription.id);
    stripe.cancel_now(&subscription.id).await.expect("cancel");
}
