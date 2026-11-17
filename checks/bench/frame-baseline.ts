// Full baseline Canvas/view work. No terminal, window, GPU or service is started.
// This is an elapsed draw+hash metric, never native input-to-present latency.
import { readFileSync } from "node:fs"
import { performance } from "node:perf_hooks"
import { Canvas } from "../../oracle/src/canvas.ts"
import { draw, initialState, observe, pick } from "../../oracle/src/view.ts"
import { derivePalette } from "../../oracle/src/theme.ts"
import { THEMES } from "../../oracle/test/themes.ts"
import type { Snapshot } from "../../oracle/src/source.ts"
import type { Provider } from "../../oracle/src/usage.ts"

const args = process.argv.slice(2)
function option(name: string, fallback?: string): string {
  const index = args.indexOf(name)
  if (index < 0 && fallback !== undefined) return fallback
  if (index < 0 || !args[index + 1] || args[index + 1].startsWith("--")) throw new Error(`missing ${name}`)
  return args[index + 1]
}
const grid = /^(\d+)x(\d+)$/.exec(option("--grid", "233x55"))
if (!grid) throw new Error("--grid must be WxH")
const columns = Number(grid[1]), lines = Number(grid[2])
if (columns < 48 || lines < 14 || columns > 1000 || lines > 1000) throw new Error("grid outside supported bounds")
let snapshot: Snapshot = JSON.parse(readFileSync(option("--snapshot"), "utf8"))
const usage: Provider[] = JSON.parse(readFileSync(option("--usage"), "utf8"))
if (!Array.isArray(snapshot.agents) || !Number.isFinite(snapshot.at) || !Array.isArray(usage)) throw new Error("invalid snapshot/usage")
const canvas = new Canvas(columns, lines), palette = derivePalette(THEMES.tokyo)
const state = initialState(2)
state.calm = true; state.demo = true; state.follow = false; state.bootAt = 0
observe(state, snapshot, snapshot.at)
let frame = draw(canvas, state, snapshot, usage, palette, snapshot.at)

// DJB2 xor variant, row-major, fields interleaved cp/fg/bg/attr for EVERY cell.
// imul has the same wrapping low 32 bits as ((hash*33)^field) >>> 0.
function outputHash(): number {
  let hash = 5381
  for (let index = 0; index < canvas.ch.length; index++) {
    hash = (Math.imul(hash, 33) ^ canvas.ch[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.fg[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.bg[index]) >>> 0
    hash = (Math.imul(hash, 33) ^ canvas.at[index]) >>> 0
  }
  return hash
}

const workload = option("--frame-workload", "repeat"), operations = Number(option("--frames", "300"))
if (!Number.isSafeInteger(operations) || operations < 1) throw new Error("frames must be positive")
for (let i=0;i<20;i++) { frame=draw(canvas,state,snapshot,usage,palette,snapshot.at); outputHash() }
let chain=5381, now=snapshot.at
const epoch=now, start=performance.now()
for (let i=0;i<operations;i++) {
  if (workload==="navigate" || workload==="scroll") {
    const index=Math.max(0,frame.rows.findIndex(r=>r.id===state.selected))
    const next=workload==="navigate" ? (i%2===0 ? Math.min(index+1,frame.rows.length-1) : Math.max(0,index-1)) :
      i+2===operations ? frame.rows.length-1 : i+1===operations ? 0 : Math.min(index+1,frame.rows.length-1)
    if (frame.rows[next] && frame.rows[next].id!==state.selected) pick(state,frame.rows[next].id,now)
  } else if (workload==="clock" || workload==="clock10s") now=epoch+(i+1)*(workload==="clock"?100:10000)
  else if (workload==="host") now=epoch+(i+1)*100
  else if (workload==="delta" || workload==="burst") {
    snapshot={...snapshot,agents:[...snapshot.agents]}
    const length=snapshot.agents.length
    const count=workload==="burst"?Math.ceil(length/100):Number(length>0)
    for(let j=0;j<count;j++) {
      const index=(i*count+j)%length
      const a=structuredClone(snapshot.agents[index])
      a.status=a.status==="running"?"completed":"running"
      a.events.push({t:epoch,cat:"shell",failed:false})
      snapshot.agents[index]=a
    }
    observe(state,snapshot,now)
  } else if (workload==="structural") {
    snapshot={...snapshot,agents:[...snapshot.agents]}
    if(i%3===0 && snapshot.agents.length) {
      const a=structuredClone(snapshot.agents[0])
      a.id=`frame-added-${i}`; a.parent=null
      snapshot.agents.push(a)
    } else if(i%3===1) snapshot.agents.pop()
    else if(snapshot.agents.length>1) {
      const a=structuredClone(snapshot.agents[1]), parent=snapshot.agents[0].id
      a.parent=a.parent===parent?null:parent
      snapshot.agents[1]=a
    }
    observe(state,snapshot,now)
  } else if (workload==="replace") {
    snapshot=structuredClone(snapshot)
    if(snapshot.agents.length) {
      const a=snapshot.agents[i%snapshot.agents.length]
      a.status=a.status==="running"?"completed":"running"
      a.events.push({t:epoch,cat:"shell",failed:false})
    }
    observe(state,snapshot,now)
  } else if(workload!=="repeat") throw new Error("unknown workload")
  frame=draw(canvas,state,snapshot,usage,palette,now)
  for(let j=0;j<canvas.ch.length;j++) for(const field of [canvas.ch[j],canvas.fg[j],canvas.bg[j],canvas.at[j]])
    chain=(Math.imul(chain,33)^field)>>>0
}
console.log(JSON.stringify({operations,errors:0,draw_ms:performance.now()-start,output_hash:chain.toString(16).padStart(8,"0")}))
