// Saves the TypeScript oracle's expected cells for every grid-parity state, so the Rust
// tests outlive the original. Same grids, actions and setup as grid-parity.ts, via
// grid-reference.ts. Run with TZ=UTC after fixtures.ts. See fixtures/golden/README.md.
import {mkdirSync,copyFileSync,readFileSync,writeFileSync} from 'node:fs'
import {gzipSync} from 'node:zlib'
import {spawnSync} from 'node:child_process'
import {createHash} from 'node:crypto'
const out=process.argv[2]??'checks/fixtures/golden'
mkdirSync(out,{recursive:true})
const grids=['233x55','237x57','160x44','95x35','71x37','60x30']
const defaultActions=['','down','end','home,space','pick:earlier:demo-webhooks','pick:earlier:demo-webhooks,enter','a','a,a','pick:demo-billing','pick:demo-checkout','pick:demo-remote','pagedown','?']
/** A fixed home folder for fixtures that do not name their own, so a recording is not machine-specific. */
const FIXED_HOME='/synthetic/home'
/** Record every gate state of `inputs` into `file`; each snapshot is copied beside it as `<name>.json`. */
const record=(file:string,inputs:{snapshot:string,name:string}[])=>{
 const states=[]
 for(const {snapshot,name} of inputs){
  copyFileSync(snapshot,`${out}/${name}.json`)
  const fixture=JSON.parse(readFileSync(snapshot,'utf8'))
  const actions:string[]=fixture.actions??defaultActions
  // A fixture that stores home paths names the home folders to draw under (`null` unsets HOME).
  const homes:(string|null)[]=fixture.homes??[FIXED_HOME]
  for(const home of homes)for(const grid of grids)for(const steps of actions){
   const env:Record<string,string|undefined>={...process.env,TZ:'UTC'}
   if(home===null)delete env.HOME
   else env.HOME=home
   const p=spawnSync('node',['--experimental-ffi','--disable-warning=ExperimentalWarning','checks/scripts/grid-reference.ts','--snapshot',snapshot,'--grid',grid,'--actions',steps,'--json'],{encoding:'utf8',env})
   if(p.status!==0)throw Error(p.stderr)
   const b=JSON.parse(p.stdout), stream=b.ch.map((c:number,i:number)=>`${c},${b.fg[i]},${b.bg[i]},${b.at[i]}\n`).join('')
   states.push({input:name,home,grid,actions:steps,w:b.w,h:b.h,selected:b.selected,list:b.list,detail:b.detail,detailMax:b.detailMax,text:b.text,ch:b.ch,fg:b.fg,bg:b.bg,at:b.at,sha256:createHash('sha256').update(stream).digest('hex')})
  }
 }
 const data=states.map(s=>JSON.stringify(s)).join('\n')+'\n'
 writeFileSync(`${out}/${file}`,gzipSync(data,{level:9,mtime:0}))
 return {states:states.length,bytes:Buffer.byteLength(data),gzipBytes:gzipSync(data,{level:9,mtime:0}).byteLength}
}
// `argv[3]` records one named file only (`states`, `hidden` or `labels`); omit it for all.
const only=process.argv[3]
if(!only||only==='states'){
 const main=record('states.jsonl.gz',['demo','edge','empty'].map(name=>({snapshot:`checks/generated/${name}.json`,name})))
 console.log(JSON.stringify({file:'states.jsonl.gz',...main}))
}
// The hidden-character fixture is a tracked input, recorded in its own file so the
// recording that came from demo/edge/empty is never rewritten.
if(!only||only==='hidden'){
 const hidden=record('hidden.jsonl.gz',[{snapshot:'checks/fixtures/hidden.json',name:'hidden'}])
 console.log(JSON.stringify({file:'hidden.jsonl.gz',...hidden}))
}
// The label fixture names its own home folders (a plain one, one with regular-expression
// characters, and unset), and its own action list: every status agent's details pane.
if(!only||only==='labels'){
 const labels=record('labels.jsonl.gz',[{snapshot:'checks/fixtures/labels.json',name:'labels'}])
 console.log(JSON.stringify({file:'labels.jsonl.gz',...labels}))
}
