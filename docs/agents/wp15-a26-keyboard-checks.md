# WP15 A26 keyboard and focus check — 2026-09-26

Environment: isolated `wp15` Compose stack, Chromium at 1280 × 860, Czech demo shop. These are the browser interactions exercised and inspected separately from the Lighthouse/axe gate.

| Path | Keys and observed focus | Outcome |
| --- | --- | --- |
| Product to order | Tab to the selected colour, ArrowRight to Popelavá, Tab to Add to cart, Tab to the cart dialog's checkout button, then Tab through contact and address. Tab into shipping and ArrowRight to PPL; Tab into payment and ArrowRight to COD; Space checks consents and Enter places the order. Focus assertions cover each target. | Order confirmation shown; `keyboard only: product to cart to COD checkout` passed. |
| Pickup and payment | Tab to the pickup radio, Space opens Packeta. Tab reaches its first point, Shift+Tab reaches Close, Tab returns to the first point, Tab reaches Z-BOX, and Enter selects it. The checkout is inert while the widget is open. Tab reaches Test payment after it closes. | Pickup point and payment are selected; `keyboard reaches pickup selection and payment after modal focus returns` passed. |
| Admin product edit | Tab reaches Brand and Save product, with focus asserted before typing and submitting. | The saved value survives reload; `keyboard only: edits and saves a product` passed in the first full e2e run. |

The checks use real keyboard events and browser focus assertions. An agent-operated browser inspection also captured the focused Packeta point and payment radio. Both matched `:focus-visible`; the screenshots showed a clear blue focus outline on each control (`/tmp/wp15-pickup-focus.png`, `/tmp/wp15-payment-control.png`).
