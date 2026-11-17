// Prints the collation table `terminal::locale_compare` uses, derived from this Node's ICU the
// way `String.prototype.localeCompare` compares (default locale, sensitivity "variant").
//
// Node's ICU gives every printable ASCII character one collation element: a primary weight and
// a tertiary weight (case). Secondary weights are all zero for ASCII. `sensitivity: "base"`
// isolates the primary order, and sorting each primary tie group with `sensitivity: "variant"`
// gives the tertiary order inside it. This covers the strings the delegation comparator sees
// (ISO timestamps and ids); it does not model non-ASCII collation.
const chars = Array.from({ length: 0x7f - 0x20 }, (_, i) => String.fromCharCode(0x20 + i))
const base = new Intl.Collator(undefined, { sensitivity: 'base' })
const variant = new Intl.Collator(undefined, { sensitivity: 'variant' })

const groups = []
for (const c of chars) {
  const group = groups.find((g) => base.compare(g[0], c) === 0)
  if (group) group.push(c)
  else groups.push([c])
}
groups.sort((a, b) => base.compare(a[0], b[0]))

const primary = new Map()
const tertiary = new Map()
groups.forEach((group, i) => {
  group.sort((a, b) => variant.compare(a, b))
  group.forEach((c, j) => {
    primary.set(c, i)
    tertiary.set(c, j)
  })
})

console.log(`// BEGIN GENERATED COLLATION (Node ${process.versions.node}, ICU ${process.versions.icu})`)
console.log('/// (code point, primary weight, tertiary weight) for U+0020..U+007E.')
console.log('pub(crate) static COLLATION: &[(u32, u16, u8)] = &[')
for (const c of chars) {
  console.log(`    (${c.charCodeAt(0)}, ${primary.get(c)}, ${tertiary.get(c)}),`)
}
console.log('];')
console.log('// END GENERATED COLLATION')
