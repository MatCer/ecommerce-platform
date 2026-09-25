// Templates require accountant approval before real use (spec A17).
#let d = json("data.json")
#let tr(cs, sk, en) = if d.locale == "sk" { sk } else if d.locale == "en" { en } else { cs }
#let credit = d.kind == "credit_note"
#let vat = d.vat_payer
#let title = if credit {
  tr("Opravný daňový doklad (dobropis)", "Opravný doklad (dobropis)", "Credit note")
} else if vat {
  tr("Faktura – daňový doklad", "Faktúra – daňový doklad", "Invoice – tax document")
} else { tr("Faktura", "Faktúra", "Invoice") }
#let money(value) = [#value #d.currency]
#let label(value) = text(size: 8pt, fill: rgb("555555"), value)
#let detail(key, value) = if value != none and value != "" { [#label(key) #h(3pt) #value#linebreak()] }
#let party(person, supplier: false) = {
  strong(person.name)
  linebreak()
  for address in person.address { [#address#linebreak()] }
  if supplier {
    v(3pt)
    detail(tr("IČO", "IČO", "Company ID"), person.company_id)
    detail(tr("DIČ", "DIČ", "Tax ID"), person.vat_id)
    detail(tr("IČ DPH", "IČ DPH", "VAT ID (SK)"), person.sk_ic_dph)
    if person.registry != none { text(size: 8pt, person.registry); linebreak() }
  }
  if person.email != none { [#person.email#linebreak()] }
  if supplier and person.phone != none { [#person.phone#linebreak()] }
}
#set page(paper: "a4", margin: (x: 17mm, top: 16mm, bottom: 18mm), footer: context align(right)[
  #text(size: 8pt, fill: rgb("555555"))[#tr("Strana", "Strana", "Page") #counter(page).display("1 / 1", both: true)]
])
#set text(font: "Libertinus Serif", size: 9pt, lang: d.locale)
#set par(leading: 0.55em)
#set table(stroke: none, inset: (x: 4pt, y: 6pt))
#text(size: 20pt, weight: "bold", title)
#v(3pt)
#text(size: 13pt, d.number)
#v(5pt)
#line(length: 100%, stroke: 1.2pt + rgb("34495e"))
#v(8pt)
#grid(columns: (1fr, 1fr), gutter: 12mm,
  [#label(tr("DODAVATEL", "DODÁVATEĽ", "SUPPLIER")) #v(3pt) #party(d.supplier, supplier: true)],
  [#label(tr("ODBĚRATEL", "ODBERATEĽ", "CUSTOMER")) #v(3pt) #party(d.customer)],
)
#v(9pt)
#let payment = (
  bank_transfer: tr("Bankovním převodem", "Bankovým prevodom", "Bank transfer"),
  stripe: tr("Kartou online", "Kartou online", "Card"),
  cod: tr("Dobírka", "Dobierka", "Cash on delivery"),
  fake: tr("Testovací platba", "Testovacia platba", "Test payment"),
).at(d.payment_method)
#grid(columns: (1fr, 1fr), gutter: 12mm,
  [
    #detail(tr("Datum vystavení", "Dátum vystavenia", "Issue date"), d.issued_on)
    #if vat { detail(tr("Datum uskutečnění zdanitelného plnění", "Dátum dodania tovaru", "Date of taxable supply"), d.taxable_supply_date) }
    #detail(tr("Datum splatnosti", "Dátum splatnosti", "Due date"), d.due_on)
    #detail(tr("Objednávka", "Objednávka", "Order number"), d.order_number)
  ],
  [
    #detail(tr("Variabilní symbol", "Variabilný symbol", "Variable symbol"), d.variable_symbol)
    #detail(tr("Způsob platby", "Spôsob platby", "Payment method"), payment)
    #if d.paid { strong(tr("Uhrazeno", "Uhradené", "Paid")); linebreak() }
    #if d.bank_account != none {
      detail(tr("Bankovní účet", "Bankový účet", "Bank account"), d.bank_account.name)
      detail("IBAN", d.bank_account.iban)
      detail("BIC", d.bank_account.bic)
    }
  ],
)
#if credit {
  v(6pt)
  if d.original != none {
    [#tr("K dokladu č.", "K dokladu č.", "Relates to invoice") #strong(d.original.number)
      #tr("ze dne", "zo dňa", "of") #d.original.issued_on#linebreak()]
  }
  if d.reason != none { detail(tr("Důvod opravy", "Dôvod opravy", "Reason"), d.reason) }
}
#v(8pt)
#let headers = (
  tr("Položka", "Položka", "Item"), tr("Množství", "Množstvo", "Qty"),
  tr("Jedn. cena", "Jedn. cena", "Unit price"),
)
#let vat-headers = (tr("DPH", "DPH", "VAT rate"), tr("Základ", "Základ", "Net"), tr("DPH", "DPH", "VAT"))
#let cells = ()
#for row in d.lines {
  cells.push([#row.name #linebreak() #text(size: 7pt, fill: rgb("555555"), row.sku)])
  cells += (row.quantity, row.unit_price)
  if vat { cells += (row.vat_rate, row.net, row.vat) }
  cells.push(row.total)
}
#text(size: 8pt)[
  #table(
    columns: if vat { (2.9fr, 0.8fr, 1.2fr, 0.7fr, 1.2fr, 1fr, 1.3fr) } else { (3fr, 1fr, 1.2fr, 1.4fr) },
    align: (col, row) => if col == 0 { left } else { right },
    table.header(..(headers + (if vat { vat-headers } else { () }) + (tr("Celkem", "Celkom", "Total"),)).map(h => table.cell(fill: rgb("eeeeee"), strong(h)))),
    ..cells,
  )
]
#let recap(rows, currency) = {
  table(columns: (1fr, 1fr, 1fr, 1fr), align: right,
    table.header(..(tr("Sazba DPH", "Sadzba DPH", "VAT rate"), tr("Základ", "Základ", "Net"), tr("DPH", "DPH", "VAT"), tr("Celkem", "Celkom", "Gross")).map(h => strong(h))),
    ..rows.map(row => (row.rate, [#row.net #currency], [#row.vat #currency], [#row.gross #currency])).flatten(),
  )
}
#if vat {
  block(breakable: false)[
    #v(8pt)
    #strong(tr("Rekapitulace DPH", "Rekapitulácia DPH", "VAT summary"))
    #text(size: 8pt)[#recap(d.vat_recap, d.currency)]
  ]
  if d.czk_recap != none {
    let r = d.czk_recap
    block(breakable: false)[
      #v(7pt)
      #strong(tr("Rekapitulace DPH v CZK", "Rekapitulácia DPH v CZK", "VAT summary in CZK"))
      #linebreak()
      #text(size: 8pt)[(#tr("kurz ČNB", "kurz ČNB", "CNB rate") #r.rate CZK / #r.amount #r.currency
        #tr("ze dne", "zo dňa", "of") #r.rate_date)]
      #text(size: 8pt)[#recap(r.rows, "CZK")]
      #align(right)[#tr("DPH celkem", "DPH celkom", "Total VAT"): #strong([#r.vat CZK])]
    ]
  }
} else {
  v(8pt)
  tr("Dodavatel není plátcem DPH.", "Dodávateľ nie je platiteľom DPH.", "The supplier is not registered for VAT.")
}
#block(width: 100%, breakable: false)[
  #v(9pt)
  #align(right)[
    #if vat {
      detail(tr("Celkem bez DPH", "Celkom bez DPH", "Total net"), money(d.totals.net))
      detail(tr("DPH celkem", "DPH celkom", "Total VAT"), money(d.totals.vat))
    }
    #v(3pt)
    #text(size: 13pt, weight: "bold")[
      #if credit { tr("Celkem k vrácení", "Celkom na vrátenie", "Total to refund") } else { tr("Celkem k úhradě", "Celkom na úhradu", "Total") }
      #h(8pt) #money(d.totals.gross)
    ]
  ]
]
