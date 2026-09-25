// Templates require accountant approval before real use (spec A17).
#let d = json("data.json")
#let tr(cs, sk, en) = if d.locale == "sk" { sk } else if d.locale == "en" { en } else { cs }
#set page(paper: "a4", margin: 18mm)
#set text(font: "Libertinus Serif", size: 10pt, lang: d.locale)
#set par(leading: 0.65em)
#for (index, order) in d.orders.enumerate() {
  if index > 0 { pagebreak() }
  align(right, text(size: 12pt, weight: "bold", d.shop))
  text(size: 13pt, tr("Balicí list", "Baliaci list", "Packing slip"))
  v(4pt)
  text(size: 28pt, weight: "bold", order.number)
  v(8pt)
  line(length: 100%, stroke: 1.2pt + rgb("34495e"))
  v(12pt)
  grid(columns: (1fr, 1fr), gutter: 12mm,
    [#strong(tr("Doručovací adresa", "Doručovacia adresa", "Delivery address")) #v(4pt)
      #for line in order.address { [#line#linebreak()] }
      #v(4pt) #order.email
      #if order.phone != none { linebreak(); order.phone }],
    [#strong(tr("Doprava", "Doprava", "Shipping")) #v(4pt) #order.shipping
      #if order.pickup_point != none {
        v(6pt)
        strong(tr("Výdejní místo", "Výdajné miesto", "Pickup point"))
        linebreak(); order.pickup_point
      }
      #v(6pt) #tr("Datum objednávky", "Dátum objednávky", "Order date"): #order.placed_on],
  )
  v(14pt)
  if order.notes != none {
    block(fill: rgb("eeeeee"), inset: 8pt, width: 100%)[#strong(tr("Poznámka", "Poznámka", "Notes")): #order.notes]
    v(8pt)
  }
  table(columns: (16pt, 1.1fr, 3fr, 0.8fr), stroke: none, inset: (x: 4pt, y: 9pt),
    align: (col, row) => if col == 3 { right } else { left },
    table.header(..("", "SKU", tr("Položka / varianta", "Položka / variant", "Item / options"), tr("Množství", "Množstvo", "Quantity")).map(h => table.cell(fill: rgb("eeeeee"), strong(h)))),
    ..order.lines.map(row => (
      box(width: 9pt, height: 9pt, stroke: 0.7pt), row.sku,
      [#strong(row.name) #if row.options != "" { linebreak(); text(size: 9pt, row.options) }], row.quantity,
    )).flatten(),
  )
}
