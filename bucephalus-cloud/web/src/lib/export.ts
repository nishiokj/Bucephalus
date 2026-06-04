export type CsvCell = string | number | boolean | null | undefined
export type CsvRow = Record<string, CsvCell>

export function downloadCsv(filename: string, rows: CsvRow[]) {
  const columns = Array.from(
    rows.reduce((keys, row) => {
      Object.keys(row).forEach((key) => keys.add(key))
      return keys
    }, new Set<string>()),
  )
  const csv = [
    columns.map(escapeCsvCell).join(","),
    ...rows.map((row) => columns.map((column) => escapeCsvCell(row[column])).join(",")),
  ].join("\n")

  const blob = new Blob([csv], { type: "text/csv;charset=utf-8" })
  const url = URL.createObjectURL(blob)
  const link = document.createElement("a")
  link.href = url
  link.download = filename
  link.click()
  URL.revokeObjectURL(url)
}

function escapeCsvCell(value: CsvCell) {
  const text = value == null ? "" : String(value)
  if (!/[",\n\r]/.test(text)) return text
  return `"${text.replace(/"/g, '""')}"`
}
