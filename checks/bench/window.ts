// Actual baseline App host with a synthetic, in-memory, read-only source.
// Launch ONLY inside omabox, in a terminal; never the user's desktop.
import { readFileSync, writeFileSync, openSync, writeSync } from "node:fs"
import { performance } from "node:perf_hooks"
import { App } from "../../oracle/src/app.ts"
import {demoUsage} from "../../oracle/src/demo.ts"
import {derivePalette} from "../../oracle/src/theme.ts"
import {THEMES} from "../../oracle/test/themes.ts"
import { Source, type Snapshot } from "../../oracle/src/source.ts"

const args = process.argv.slice(2)
const argsForWindow = args
function option(name: string): string | undefined {
  const index = args.indexOf(name)
  return index < 0 ? undefined : args[index + 1]
}
const path = option("--snapshot")
if (!path) throw new Error("--snapshot required; this runner never opens a T3 database")
const initial: Snapshot = JSON.parse(readFileSync(path, "utf8"))
const replayPath = option("--replay")
const replay: Snapshot[] = replayPath ? JSON.parse(readFileSync(replayPath, "utf8")) : [initial]
if (!Array.isArray(replay) || !replay.length) throw new Error("nonempty snapshot replay required")
const launched = performance.now()
const current = () => replay[Math.min(replay.length - 1, Math.floor((performance.now() - launched) * 0.01))]
// Both native and baseline draw the same snapshot.at virtual time. Original App
// timers still use their production/adaptive rates; only the view's wall clock
// and data timestamps are controlled. No tool items or private records loaded.
Date.now = () => current().at
class SyntheticSource extends Source {
  constructor() { super("synthetic:shared-fixture") }
  snapshot(): Snapshot { return current() }
}
const suppliedUsage=option("--usage")?JSON.parse(readFileSync(option("--usage")!,"utf8")):undefined
const app = new App(new SyntheticSource(), 2, { demo: true, calm: true, usage: { poll: () => suppliedUsage??(args.includes("--full-visual") ? demoUsage(current().at,initial.at) : []) } })
if(args.includes("--full-visual")){app.theme.palette=derivePalette(THEMES.tokyo);app.theme.poll=()=>{}}
// Opt-in markers share CLOCK_MONOTONIC with the Wayland presentation probe.
const presentationFd=process.env.T3_PRESENT_TRACE ? openSync(process.env.T3_PRESENT_TRACE,"a",0o600) : undefined
let inputSeq=0
type PreparedFrame={input_seq:number,frame_hash:string,columns:number,lines:number,selected:string|null,scope:string,prepared_mono_ns:number}
let preparedFrame:PreparedFrame|undefined
let taggedHash:string|undefined
let frameListener:App['renderer']=null
function presentMarker(type:string,frame=false){
 if(presentationFd===undefined)return
 let frame_hash:string|undefined
 if(frame){let hash=5381;const c=app.canvas;for(let i=0;i<c.ch.length;i++)for(const v of [c.ch[i],c.fg[i],c.bg[i],c.at[i]])hash=(Math.imul(hash,33)^v)>>>0;frame_hash=hash.toString(16).padStart(8,"0")}
 const stamp=Number(process.hrtime.bigint())
 if(frame)preparedFrame={input_seq:inputSeq,frame_hash:frame_hash!,columns:app.canvas.w,lines:app.canvas.h,selected:app.st.selected,scope:app.st.scope,prepared_mono_ns:stamp}
 writeSync(presentationFd,JSON.stringify({type,pid:process.pid,mono_ns:stamp,input_seq:inputSeq,selected:app.st.selected,scope:app.st.scope,...(frame?{columns:app.canvas.w,lines:app.canvas.h,frame_hash}: {})})+"\n")
}
function installOutputTag(){
 if(presentationFd===undefined||!app.renderer||frameListener===app.renderer)return
 frameListener=app.renderer
 // OpenTUI emits this public event after presentNativeScene/whenPresented.
 // Its single-rendering guard preserves the immutable descriptor from that paint
 // while output awaits completion, even if another input changes App.st meanwhile.
 frameListener.on("frame",(event:any)=>{
  const frame=preparedFrame
  if(!frame||frame.frame_hash===taggedHash)return
  const tag=`\x1b]99999;t3-present;${frame.input_seq};${frame.frame_hash};${frame.columns};${frame.lines}\x07`
  writeSync(1,tag) // bypass OpenTUI's stdout capture; an ignored, private OSC fence
  taggedHash=frame.frame_hash
  writeSync(presentationFd,JSON.stringify({type:"app_output",pid:process.pid,...frame,mono_ns:Number(process.hrtime.bigint()),sdk_frame_id:event.frameId})+"\n")
 })
}
let writtenGeometry=""
function geometryState() {
  const path=process.env.T3_EXPERIMENT_GEOMETRY
  if(!path)return
  const geometry=JSON.stringify({columns:app.canvas.w,lines:app.canvas.h})
  // One write after the first paint, and again only on resize. Do not add
  // recurring telemetry I/O to the baseline's production adaptive scheduler.
  if(geometry!==writtenGeometry){writeFileSync(path,geometry);writtenGeometry=geometry}
}
function interactionState() {
  if (process.env.T3_EXPERIMENT_INTERACTION && app.frame) writeFileSync(process.env.T3_EXPERIMENT_INTERACTION,
    JSON.stringify({selected:app.st.selected,scope:app.st.scope,rows:app.frame.rows.map(r=>r.id)}))
}
const paint=app.paint.bind(app)
app.paint=(...paintArgs)=>{installOutputTag();paint(...paintArgs);presentMarker("app_frame",true);geometryState();interactionState();if(option("--dump-frame"))writeFileSync(option("--dump-frame")!,JSON.stringify({w:app.canvas.w,h:app.canvas.h,text:app.canvas.toText(),selected:app.st.selected,list:app.frame?.list,detail:app.frame?.detail}))}
const key=app.key.bind(app)
app.key=(...args)=>{key(...args);if(presentationFd!==undefined){inputSeq++;presentMarker("app_key")}interactionState();
 // Separate, explicit scheduler control: one immediate input frame through the
 // public API. The original App.paint retains its adaptive idle/motion rate.
 if(argsForWindow.includes("--input-immediate"))app.renderer?.intermediateRender()}
let previous=0,applied=0,skipped=0
let timer: ReturnType<typeof setInterval> | undefined
if (replayPath) timer = setInterval(() => {
  const index=Math.min(replay.length-1,Math.floor((performance.now()-launched)*0.01))
  if (!app.st.paused && index>previous) {
    app.refresh()
    if(index>previous){applied++;skipped+=Math.max(0,index-previous-1);previous=index}
    if (process.env.T3_EXPERIMENT_METRICS) writeFileSync(process.env.T3_EXPERIMENT_METRICS,
      JSON.stringify({index: Math.min(replay.length-1,Math.floor((performance.now()-launched)*0.01)), at: app.snap.at, agents:app.snap.agents.length,updates_applied:applied,updates_skipped:skipped,errors:app.st.error ? 1 : 0}))
    if(index===replay.length-1 && timer) clearInterval(timer)
  }
}, 100)
try { await app.run() } finally { if (timer) clearInterval(timer) }
