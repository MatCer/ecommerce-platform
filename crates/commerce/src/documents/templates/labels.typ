// Templates require accountant approval before real use (spec A17).
#let d = json("data.json")
#set page(width: 105mm, height: 148mm, margin: 0pt)
#for (index, file) in d.files.enumerate() {
  if index > 0 { pagebreak() }
  image(file, width: 100%, height: 100%, fit: "contain")
}
