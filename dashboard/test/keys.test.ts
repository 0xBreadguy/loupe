// Numpad keys act like their main-keyboard twins.
import assert from "node:assert/strict"
import { test } from "node:test"
import { keyName } from "../src/keys.ts"

test("numpad Enter and navigation keys map to the main keys", () => {
  assert.equal(keyName("kpenter"), "return")
  assert.equal(keyName("kpdown"), "down")
  assert.equal(keyName("kppagedown"), "pagedown")
  assert.equal(keyName("return"), "return")
  assert.equal(keyName("kp5"), "kp5", "digits are left alone")
  assert.equal(keyName(undefined), "")
})
