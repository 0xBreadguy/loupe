// Independent host/motion parity gate; a successful comparison never claims video playback review.
import assert from 'node:assert/strict'
import {spawnSync} from 'node:child_process'
import {readFileSync,writeFileSync,mkdirSync} from 'node:fs'
import {resolve} from 'node:path'
import {createHash} from 'node:crypto'
import {numbersDigest} from './golden.ts'
// Opt-in: with T3_RECORD set, export-behavior.ts turns this run into the recorded fixture.
import {enabled as recording, recordAsset, recordCase} from './record.ts'
const root=resolve('checks'),generated=resolve(root,'generated/behavior')
mkdirSync(generated,{recursive:true})
const snapshot=JSON.parse(readFileSync(resolve(root,'fixtures/demo.json'),'utf8'))
const binary=resolve(process.argv[2]??process.env.T3_NATIVE_BINARY??'target/release/t3-agents-core')
const epoch=snapshot.at
const copy=<T>(x:T):T=>JSON.parse(JSON.stringify(x))
const update=(ms:number,edit:(snapshot:any)=>void)=>{const next=copy(snapshot);next.at=epoch+ms;edit(next);return {ms,snapshot:next}}
const find=(s:any,id:string)=>s.agents.find((a:any)=>a.id===id)
const add=(s:any,kind:string,id:string)=>{const a=find(s,id);a.items.unshift({thread_id:a.thread_id,run_id:a.run?.id??null,node_id:`new-${kind}`,type:kind,status:'completed',updated_at:new Date(s.at).toISOString(),started_at:new Date(s.at-100).toISOString(),command:'cargo test synthetic-new',file:'src/new.rs',label:'synthetic arrival',tool:null})}
const cases:any[]=[
 {name:'clock-and-retarget',calm:false,events:[{ms:16},{ms:32},{ms:100},{ms:249},{ms:300},update(320,s=>{find(s,'demo-search').context.usedTokens=850000}),{ms:336},{ms:350},update(365,s=>{find(s,'demo-search').context.usedTokens=250000}),{ms:381},{ms:400},{ms:650},{ms:900},{ms:1500},{ms:1000},{ms:2000}]},
 {name:'help-shortcuts-and-freeze',calm:false,events:[{ms:1000,key:'?'},{ms:1500,key:'down'},{ms:1800,key:'pagedown'},{ms:2000,key:'a'},{ms:2100,key:'p'},{ms:2200,key:'p'},{ms:2300,key:'t'},{ms:2400,key:'f'},{ms:2500,key:'m'},{ms:2600,key:'x'},update(2700,s=>add(s,'command_execution','demo-search')),{ms:3000},{ms:3100,key:'escape'},{ms:3300},{ms:3500,key:'m'},{ms:3600},{ms:4000}]},
 {name:'arrival-and-acknowledgement',calm:false,events:[{ms:1000,pick:'demo-search'},update(1100,s=>add(s,'user_message','demo-search')),update(1200,s=>add(s,'command_execution','demo-search')),{ms:1210},{ms:1450},{ms:1650},{ms:2200},update(2300,s=>{find(s,'demo-search').requests=[];find(s,'demo-search').status='running'}),{ms:2400,pick:'demo-search'},update(2500,s=>{find(s,'demo-search').requests=['user input'];find(s,'demo-search').status='waiting'}),{ms:2800},{ms:2900,pick:'demo-search'},{ms:3500}]},
 {name:'notices-parent-and-expiry',calm:true,events:[{ms:1000,pick:'demo-search-native'},{ms:1100,key:'enter'},{ms:1200,key:'m'},{ms:1300,key:'m'},{ms:1400,key:'x'},{ms:1500,key:'down'},{ms:5500},{ms:5600,key:'a'},{ms:5700,key:'?'},{ms:5800,key:'a'},{ms:5900,key:'?'},{ms:10000}]},
 {name:'calm-and-reduced-override',calm:false,events:[{ms:50},{ms:100,reduced:true},{ms:200},{ms:300,key:'m'},{ms:400,key:'x'},{ms:500,key:'m'},{ms:600},{ms:700,reduced:false},{ms:800},{ms:900,key:'m'},{ms:1000}]},
 {name:'quiet-arrival-scheduler',calm:false,events:[update(1000,s=>{for(const a of s.agents){a.status='completed';a.requests=[];a.now=null} find(s,'demo-search').items=[]}),update(1200,s=>{for(const a of s.agents){a.status='completed';a.requests=[];a.now=null} add(s,'command_execution','demo-search')}),{ms:1300},{ms:1700},{ms:2801},{ms:4000}]},
 {name:'crt-phase-toggles-and-help',calm:false,events:[{ms:1000,key:'x'},{ms:1100},{ms:1200,key:'?'},{ms:1800},{ms:2000,key:'x'},{ms:2400},{ms:2600,key:'x'},{ms:3000,key:'escape'},{ms:3100,key:'m'},{ms:3500},{ms:4000,key:'m'},{ms:4100,key:'x'},{ms:4200,reduced:true},{ms:4500,reduced:false}]},
 {name:'pause-refresh-latest',calm:true,events:[{ms:1000,key:'p'},update(1100,s=>{find(s,'demo-search').title='synthetic replacement';add(s,'command_execution','demo-search')}),{ms:1200,key:'r'},{ms:1300,key:'p'},{ms:1400,key:'r'},{ms:1500},{ms:3000}]},
]
const validIds=new Set(snapshot.agents.map((a:any)=>a.id))
const child=snapshot.agents.find((a:any)=>a.parent&&a.thread_id!==a.id)
for(const c of cases)for(const e of c.events)if(e.pick&&!validIds.has(e.pick))e.pick=child.id
const fields=['index','now','semantic','calm','paused','help','fx','follow','windowIndex','detailScroll','helpScroll','notice','picked','firstSeen','shimmer','acked','waits','frozenAt','fps']
let frames=0,cellCases=0,errors=0
const findings:any[]=[]
for(const c of cases){
 const spec={width:160,height:44,...c},path=resolve(generated,c.name+'.json');writeFileSync(path,JSON.stringify(spec))
 const run=(cmd:string,args:string[])=>{const p=spawnSync(cmd,args,{encoding:'utf8',env:{...process.env,TZ:'UTC'},maxBuffer:128*1024*1024});assert.equal(p.status,0,`${cmd}: ${p.stderr}`);return JSON.parse(p.stdout).frames}
 const common=['--snapshot',resolve(root,'fixtures/demo.json'),'--behavior-script',path]
 const expected=run('node',['--experimental-ffi','--disable-warning=ExperimentalWarning',resolve(root,'scripts/behavior-reference.ts'),...common])
 const actual=run(binary,common)
 if(recording){
  const recorded=expected.map((f:any)=>({...Object.fromEntries(fields.map((field:string)=>[field,f[field]])),fxPosition:f.fxPosition,
   ch:numbersDigest(f.ch),fg:numbersDigest(f.fg),bg:numbersDigest(f.bg),attr:numbersDigest(f.attr)}))
  // firstSeen is a per-agent arrival clock that repeats nearly unchanged; keep it only where it
  // changes and let the test carry the previous frame's copy forward.
  for(let i=1;i<recorded.length;i++)if(JSON.stringify(recorded[i].firstSeen)===JSON.stringify(recorded[i-1].firstSeen))delete recorded[i].firstSeen
  // The script travels as the exact bytes the oracle read: the original's `mix` maps keep JSON
  // insertion order for tied tool counts, so re-serializing the file would change the screen.
  recordCase({kind:'behavior',case:c.name,spec:recordAsset(path,'text'),snapshot:recordAsset(resolve(root,'fixtures/demo.json'),'text'),frames:recorded})
 }
 assert.equal(actual.length,expected.length)
 for(let i=0;i<expected.length;i++){
  frames++
  const b=expected[i],n=actual[i]
  for(const field of fields){try{assert.deepEqual(n[field],b[field])}catch{errors++;findings.push({case:c.name,frame:i,ms:c.events[i-1]?.ms??0,field,baseline:b[field],native:n[field]})}}
  if(Math.abs(n.fxPosition-b.fxPosition)>1e-12){errors++;findings.push({case:c.name,frame:i,field:'fxPosition',baseline:b.fxPosition,native:n.fxPosition})}
  try{assert.deepEqual(n.tween.map((p:any)=>p[0]),b.tween.map((p:any)=>p[0]));for(let j=0;j<n.tween.length;j++)assert.ok(Math.abs(n.tween[j][1]-b.tween[j][1])<1e-12)}catch{errors++;findings.push({case:c.name,frame:i,field:'tween',baseline:b.tween,native:n.tween})}
  // Original CRT now runs through its real CPU buffer and postprocess pipeline.
  {cellCases++;for(const field of ['ch','fg','bg','attr']){let count=0;const first:any[]=[];for(let j=0;j<b[field].length;j++)if(n[field][j]!==b[field][j]){count++;if(first.length<8)first.push({x:j%spec.width,y:Math.floor(j/spec.width),baseline:b[field][j],native:n[field][j]})}if(count){errors++;findings.push({case:c.name,frame:i,ms:c.events[i-1]?.ms??0,field,differences:count,first})}}}
 }
}
const sha=(p:string)=>createHash('sha256').update(readFileSync(p)).digest('hex')
const report={cases:cases.map(c=>({name:c.name,events:c.events.length})),frames,cellCases,errors,native_sha256:sha(binary),snapshot_sha256:sha(resolve(root,'fixtures/demo.json')),findings}
writeFileSync(resolve(generated,'report.json'),JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({frames,cellCases,errors,findings:findings.slice(0,24)},null,2))
if(errors)process.exitCode=1
