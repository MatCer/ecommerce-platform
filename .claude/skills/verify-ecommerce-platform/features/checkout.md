# Checkout and payment

A shopper hands the cart to the platform checkout on `checkout.<shop>`, enters contact and
address, picks shipping and payment, places the order, pays at the fake gateway and lands on the
order page, with a confirmation email in Mailpit. The same-origin checkout (`<shop>/checkout`)
runs on the `demo-fresh(-sk).localhost` origins.

## Sub-features

- `co-handoff` moves the shop cart to the checkout origin; the handoff link is single use.
- `co-guest` places a guest order with home delivery or a Packeta pickup point.
- `co-pay` pays through the fake gateway (Pay / Fail the payment, retry).
- `co-order` shows status, payment and pickup point on `/o/<token>` and emails a confirmation.
- `co-same-origin` runs the native-form checkout with review on the shop origin.

## How to get to it (user POV)

- From the cart drawer, `button "K pokladně"`.
- Same-origin: `http://demo-fresh.localhost:38080/`, add to cart, start checkout; lands on
  `/checkout` on the same origin.

## Driving it with Playwright

Preconditions:

- Held stack up; the e2e fixture setup ran (`stack.sh up` does it), so the five checkout products
  have stock and the same-origin origins exist.
- A fresh context with the consent cookie for `demo.localhost:38080` and `demo-sk.localhost:38080`.

- **Spec run.** Run `stack.sh e2e 3 --project chromium --no-deps checkout/orders.spec.ts -g "guest checkout in CZ"`.
  For the handoff in Firefox: `stack.sh e2e 3 --project firefox --no-deps checkout/handoff.spec.ts`.
- **Handoff.** Add `tricko-henley` from its product page and click `button "K pokladně"`.
  `heading "Pokladna" [level=1]` appears on `checkout.demo.localhost:38080`, and
  `getByTestId("checkout-total")` shows the cart total.
- **Details.** Fill `input[autocomplete="email"]` (a unique `@example.test` address),
  `name`, `street-address`, `postal-code`, `address-level2`. Check `radio /PPL – kurýr/` and
  `radio /Testovací platba/`.
- **Place.** Tick `checkbox /^Souhlasím s obchodn/` and `checkbox /^Beru na vědomí/` by clicking
  their text, then `button "Objednat s povinností platby"`. The URL becomes `/_p/fake-pay/<attempt>`
  with `heading "Test payment"`.
- **Pay.** Click `button "Pay"` (exact). The URL becomes `/o/<64 hex>`; `order-status` reads
  `Potvrzená` and `payment-status` reads `Zaplaceno`.
- **Second view.** `GET http://checkout.demo.localhost:38080/_p/orders/<token>` returns
  `status: "confirmed"`, `payment.status: "paid"`; Mailpit
  (`http://127.0.0.1:38025/api/v1/search?query=to:"<email>"`) has `Potvrzení objednávky <number>`.
- **Proof.** Screenshot and ARIA snapshot of the checkout before placing and of the order page;
  save the order JSON and the email text.

## Gotchas

- Placing with nothing chosen shows `alert` with `Vyplňte prosím`; that is the validation path,
  not a failure.
- Carts do not reserve stock; a parallel run can take the last unit. Rerun the fixture setup
  (`stack.sh e2e 3 --project chromium-setup --no-deps`), which tops stock up, rather than editing
  stock by hand.
- The handoff link is single use: reopening it shows `Odkaz na pokladnu vypršel nebo už byl použit.`
- Unpaid fake orders expire after 60 minutes and release stock.
- In Firefox, extra tabs on the shop or checkout must go through `/favicon.svg` first (COOP).
- WebKit drops `Secure` cookies over plain HTTP; WebKit cookie flows need `https://demo-fresh.localhost:38443`.
