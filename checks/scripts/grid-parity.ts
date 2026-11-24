// Independent text + color parity gate. Both implementations render the same full fixture and usage.
// A fixture may name the grids, actions or home folders to draw under; otherwise the defaults below.
import {spawnSync} from 'node:child_process'
import {writeFileSync,mkdirSync,readFileSync} from 'node:fs'
import {createHash} from 'node:crypto'
import {resolve} from 'node:path'
const binary=resolve(process.env.T3_NATIVE_BINARY??'target/release/loupe-core')
const snapshot=resolve(process.argv[2]??'checks/fixtures/demo.json')
// Both sides run in the same zone. GATE_TZ picks another one: clock labels are local time.
const zone=process.env.GATE_TZ??'UTC'
const fixture=JSON.parse(readFileSync(snapshot,'utf8'))
// GATE_GRIDS draws at other sizes than the fixture's or the defaults: a comma list, where an
// entry like 60..300x58 means every width in that range at one height. GATE_ACTIONS, separated
// by |, replaces the actions. Together they make a width sweep: a meter's colors depend on its
// width, and a rounding tie in a color shows at a few widths only (it did at 182 to 188).
const widths=(g:string)=>{const m=/^(\d+)\.\.(\d+)x(\d+)$/.exec(g);return m?Array.from({length:+m[2]-+m[1]+1},(_,i)=>`${+m[1]+i}x${m[3]}`):[g]}
const grids: string[] = process.env.GATE_GRIDS ? process.env.GATE_GRIDS.split(',').flatMap(widths) : fixture.grids ?? ['233x55','237x57','160x44','95x35','71x37','60x30']
const actions: string[] = process.env.GATE_ACTIONS !== undefined ? process.env.GATE_ACTIONS.split('|') : fixture.actions ?? ['','down','end','home,space','pick:earlier:demo-webhooks','pick:earlier:demo-webhooks,enter','a','a,a','pick:demo-billing','pick:demo-checkout','pick:demo-remote','pagedown','?']
// `homes` pins HOME for both programs (null unsets it): labels shorten the home folder, so a
// fixture that stores home paths is drawn once under each. Absent means this machine's HOME.
const homes: (string|null|undefined)[] = fixture.homes ?? [undefined]
let states=0,textDifferences=0,colorDifferences=0,attributeDifferences=0
const mismatches:any[]=[]
for(const home of homes)for(const grid of grids)for(const steps of actions){
 const env:Record<string,string|undefined>={...process.env,TZ:zone}
 if(home===null)delete env.HOME
 else if(home!==undefined)env.HOME=home
 const invoke=(cmd:string,args:string[])=>{const p=spawnSync(cmd,args,{encoding:'utf8',env,maxBuffer:8*1024*1024});if(p.status!==0)throw Error(`${cmd}: ${p.stderr}`);return JSON.parse(p.stdout)}
 const base=invoke('node',['--experimental-ffi','--disable-warning=ExperimentalWarning','checks/scripts/grid-reference.ts','--snapshot',snapshot,'--grid',grid,'--actions',steps,'--json'])
 const native=invoke(binary,['--snapshot',snapshot,'--grid',grid,'--actions',steps,'--frame-json'])
 let text=0,colors=0,attributes=0
 for(let i=0;i<base.ch.length;i++){text+=base.ch[i]!==native.ch[i]?1:0;colors+=(base.fg[i]!==native.fg[i]||base.bg[i]!==native.bg[i])?1:0;attributes+=base.at[i]!==native.at[i]?1:0}
 if(text||colors||attributes){mismatches.push({grid,actions:steps,home:home===undefined?"inherit":home,text,colors,attributes});if(text){const b=base.text.split('\n'),n=native.text.split('\n');const lines=b.flatMap((v:string,i:number)=>v!==n[i]?[{y:i,baseline:v,native:n[i]}]:[]);mkdirSync('checks/generated/grid-diff',{recursive:true});writeFileSync(`checks/generated/grid-diff/${grid}-${states}.json`,JSON.stringify(lines,null,2))}}
 states++;textDifferences+=text;colorDifferences+=colors;attributeDifferences+=attributes
}
const sha=(path:string)=>createHash('sha256').update(readFileSync(path)).digest('hex')
const report={snapshot_sha256:sha(snapshot),native_sha256:sha(binary),grids,actions,homes:homes.map(h=>h===undefined?"inherit":h),states,textDifferences,colorDifferences,attributeDifferences,mismatches}
mkdirSync('checks/generated',{recursive:true});writeFileSync(process.argv[3]??'checks/generated/grid-parity.json',JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify(report,null,2))
if(textDifferences||colorDifferences||attributeDifferences)process.exitCode=1
