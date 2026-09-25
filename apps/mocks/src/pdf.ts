/** Minimal PDF 1.4 carrier label, using built-in Helvetica and byte-counted xref entries. */
export function labelPdf(carrier: string, barcode: string, recipient: string): Buffer<ArrayBuffer> {
  const literal = (value: string) =>
    value
      .normalize("NFD")
      .replace(/\p{M}/gu, "")
      .replace(/[^\x20-\x7e]/g, "?")
      .replace(/[\\()]/g, "\\$&");
  const lines = [carrier, barcode, ...(recipient.match(/.{1,32}/gu) ?? [])].slice(0, 10);
  const stream = lines
    .map(
      (line, index) =>
        `BT /F1 ${index === 1 ? 18 : 12} Tf 20 ${380 - index * 28} Td (${literal(line)}) Tj ET\n`,
    )
    .join("");
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 298 420] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    `<< /Length ${Buffer.byteLength(stream)} >>\nstream\n${stream}endstream`,
  ];
  let pdf = "%PDF-1.4\n";
  const offsets = [0];
  objects.forEach((object, index) => {
    offsets.push(Buffer.byteLength(pdf));
    pdf += `${index + 1} 0 obj\n${object}\nendobj\n`;
  });
  const xref = Buffer.byteLength(pdf);
  pdf += `xref\n0 ${offsets.length}\n0000000000 65535 f \n`;
  pdf += offsets
    .slice(1)
    .map((offset) => `${String(offset).padStart(10, "0")} 00000 n \n`)
    .join("");
  pdf += `trailer\n<< /Size ${offsets.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(pdf);
}
