// Shared finite replay for both application hosts; no real records or services.
import { writeFileSync, mkdirSync } from "node:fs"
import { ROOT, rapidSnapshot } from "./fixtures.ts"
mkdirSync(ROOT + "/generated", { recursive: true })
// 10 Hz hosts consume all120 updates over12 seconds, including sampled warmup.
writeFileSync(ROOT + "/generated/replay.json", JSON.stringify(Array.from({ length:120 },(_,i)=>rapidSnapshot(i)))+"\n")
writeFileSync(ROOT + "/generated/replay-short.json", JSON.stringify(Array.from({ length:30 },(_,i)=>rapidSnapshot(i)))+"\n")
