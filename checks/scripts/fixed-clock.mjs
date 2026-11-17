// Preload for the CLI parity gate: pin the wall clock to T3_FIXED_NOW (milliseconds).
// `new Date(argument)` and the static parse/UTC helpers still behave normally, so only
// calls that read "now" are fixed. Never imported by the dashboard itself.
const fixed = Number(process.env.T3_FIXED_NOW)
if (!Number.isFinite(fixed)) throw new Error("T3_FIXED_NOW must be a finite millisecond timestamp")
const RealDate = Date
class FixedDate extends RealDate {
  constructor(...args) {
    if (args.length === 0) super(fixed)
    else super(...args)
  }
  static now() {
    return fixed
  }
}
globalThis.Date = FixedDate
