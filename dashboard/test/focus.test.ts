// The T3 Code link: window titles to threads, and following Hyprland's events.
import assert from "node:assert/strict"
import { EventEmitter } from "node:events"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { createServer, type Server, type Socket } from "node:net"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { test } from "node:test"
import { T3Window, environmentId, hyprDir, openLink, t3Thread, threadLink, type Spawner } from "../src/focus.ts"
import type { Agent } from "../src/source.ts"

const agent = (id: string, title: string, project: string, extra: Partial<Agent> = {}) =>
  ({ id, thread_id: id, title, project, updated: "2026-11-12T00:00:00Z", ...extra }) as Agent

test("a window title names a thread by project and title, else by title alone", () => {
  const agents = [
    agent("a", "Fix the build", "dotfiles"),
    agent("b", "Fix the build", "other-project", { updated: "2026-11-13T00:00:00Z" }),
    agent("c", "Notes / ideas", "dotfiles"),
    // A native subagent has no T3 page, even when its title matches.
    agent("sub", "Explore", "dotfiles", { thread_id: "a" }),
  ]
  assert.equal(t3Thread("dotfiles / Fix the build", agents), "a", "the project disambiguates")
  assert.equal(t3Thread("Fix the build", agents), "b", "a bare title picks the newest")
  assert.equal(t3Thread("dotfiles / Notes / ideas", agents), "c", "titles may contain the separator")
  assert.equal(t3Thread("dotfiles / Explore", agents), null)
  assert.equal(t3Thread("T3 Code (Nightly)", agents), null, "no thread open")
  assert.equal(t3Thread(null, agents), null)
})

test("the Hyprland socket directory comes from the session, or there is none", () => {
  assert.equal(hyprDir({ XDG_RUNTIME_DIR: "/run/user/1", HYPRLAND_INSTANCE_SIGNATURE: "abc_1" }), "/run/user/1/hypr/abc_1")
  assert.equal(hyprDir({ XDG_RUNTIME_DIR: "/run/user/1" }), null)
  assert.equal(hyprDir({ XDG_RUNTIME_DIR: "/run/user/1", HYPRLAND_INSTANCE_SIGNATURE: "../escape" }), null)
})

const listen = (server: Server, path: string) => new Promise<void>((resolve) => server.listen(path, resolve))

test("the T3 window title follows Hyprland's title, focus and close events", { timeout: 5000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "t3a-"))
  let clients = [
    { address: "0xaa", class: "com.t3tools.T3Code", title: "proj / One", focusHistoryID: 3 },
    { address: "0xbb", class: "foot", title: "a shell", focusHistoryID: 0 },
    { address: "0xcc", class: "com.t3tools.T3Code", title: "proj / Two", focusHistoryID: 1 },
  ]
  const requests = createServer((s) => s.once("data", (d) => s.end(String(d) === "j/clients" ? JSON.stringify(clients) : "unknown request")))
  let events: Socket | null = null
  const eventServer = createServer((s) => (events = s))
  const connected = new Promise<void>((resolve) => eventServer.once("connection", () => resolve()))
  await listen(requests, join(dir, ".socket.sock"))
  await listen(eventServer, join(dir, ".socket2.sock"))

  let wake = () => {}
  const changed = () => new Promise<void>((resolve) => (wake = resolve))
  const seen: (string | null)[] = []
  const window = new T3Window((title) => {
    seen.push(title)
    wake()
  }, dir)
  try {
    let next = changed()
    window.start()
    await Promise.all([next, connected])
    assert.equal(window.title, "proj / Two", "the T3 window focused most recently")

    next = changed()
    events!.write("windowtitlev2>>cc,proj / Two, renamed\n")
    await next
    assert.equal(window.title, "proj / Two, renamed")

    next = changed()
    events!.write("activewindowv2>>bb\nactivewindowv2>>aa\n") // another app's window changes nothing
    await next
    assert.equal(window.title, "proj / One")

    next = changed()
    clients = clients.filter((c) => !c.class.includes("T3Code"))
    events!.write("closewindow>>aa\n")
    await next
    assert.equal(window.title, null)
    assert.deepEqual(seen, ["proj / Two", "proj / Two, renamed", "proj / One", null])
  } finally {
    window.stop()
    requests.close()
    eventServer.close()
    rmSync(dir, { recursive: true, force: true })
  }
})

test("without Hyprland the link stays off", () => {
  const window = new T3Window(() => assert.fail("no events expected"), null)
  window.start()
  assert.equal(window.available, false)
  assert.equal(window.title, null)
  window.stop()
})

test("thread links are built the way T3 builds them", () => {
  assert.equal(threadLink("e-1", "a7b8"), "t3code://threads/e-1/a7b8")
  // Ids may hold separators and percent signs; each is one encoded path segment.
  assert.equal(threadLink("e-1", "thread:task:a%3Ab/c"), "t3code://threads/e-1/thread%3Atask%3Aa%253Ab%2Fc")
  assert.equal(threadLink("", "x"), null)
  const dir = mkdtempSync(join(tmpdir(), "t3a-"))
  try {
    assert.equal(environmentId(join(dir, "statev2.sqlite")), null)
    writeFileSync(join(dir, "environment-id"), "0b6c7d1e-2f3a-4b5c-8d9e-0f1a2b3c4d5e\n")
    assert.equal(environmentId(join(dir, "statev2.sqlite")), "0b6c7d1e-2f3a-4b5c-8d9e-0f1a2b3c4d5e")
    writeFileSync(join(dir, "environment-id"), "../not an id")
    assert.equal(environmentId(join(dir, "statev2.sqlite")), null)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test("a link is handed to xdg-open detached, without a shell", async () => {
  const calls: unknown[][] = []
  const fake = (outcome: "spawn" | "error") => ((...args: unknown[]) => {
    calls.push(args)
    const child = Object.assign(new EventEmitter(), { unref() {} })
    queueMicrotask(() => (outcome === "spawn" ? child.emit("spawn") : child.emit("error", Object.assign(new Error("x"), { code: "ENOENT" }))))
    return child
  }) as unknown as Spawner
  await openLink("t3code://threads/e/t", fake("spawn"))
  assert.deepEqual(calls[0], ["xdg-open", ["t3code://threads/e/t"], { detached: true, stdio: "ignore" }])
  await assert.rejects(openLink("t3code://threads/e/t", fake("error")), /xdg-open is not installed/)
  await assert.rejects(openLink("https://example.invalid", fake("spawn")), /not a T3 Code link/)
  assert.equal(calls.length, 2)
})
