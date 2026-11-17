// Synthetic-only Source + full-cell differential coverage for the main parity port.
import assert from 'node:assert/strict'
import {mkdtempSync,writeFileSync,rmSync,readFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join,resolve} from 'node:path'
import {DatabaseSync} from 'node:sqlite'
import {spawnSync} from 'node:child_process'
import {buildDemo} from '../../oracle/src/demo.ts'
import {Source,PREFIX} from '../../oracle/src/source.ts'
import {span} from '../../oracle/src/format.ts'
// Opt-in: with T3_RECORD set, export-cross.ts turns this run into the recorded fixture.
import {numbersDigest} from './golden.ts'
import {enabled as recording, recordCase, recordTree} from './record.ts'
const binary=resolve(process.env.T3_NATIVE_BINARY??'target/release/t3-agents-core')
const dir=mkdtempSync(join(tmpdir(),'t3-cross-parity-'))
const now=Date.parse('2026-11-14T12:00:00Z'),iso=(t:number)=>new Date(t).toISOString()
const node=['--experimental-ffi','--disable-warning=ExperimentalWarning']
function run(cmd:string,args:string[]) {const p=spawnSync(cmd,args,{encoding:'utf8',env:{...process.env,TZ:'UTC'},maxBuffer:32*1024*1024});assert.equal(p.status,0,p.stderr);return p.stdout}
function normalized(v:any,root=false):any {
 if(Array.isArray(v))return v.map(x=>normalized(x))
 if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).filter(([k,v])=>v!==null&&!(root&&k==='path')).map(([k,v])=>[k,normalized(v)]))
 return v
}
try {
 const path=join(dir,'synthetic.sqlite');buildDemo(path,now,now);writeFileSync(join(dir,'environment-id'),'machine-a\n')
 const db=new DatabaseSync(path)
 for(const {name} of db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all() as {name:string}[])db.exec(`DELETE FROM "${name}"`)
 const thread=(id:string,extra:any={})=>db.prepare(`INSERT INTO ${PREFIX}threads(thread_id,payload_json) VALUES(?,?)`).run(id,JSON.stringify({id,title:id,...extra}))
 const ref=(environmentId:string,threadId:string)=>({environmentId,threadId})
 thread('root');thread('local',{delegatedFrom:ref('machine-a','root')})
 thread('leaf',{delegatedFrom:ref('machine-b','parent')})
 thread('unknown',{delegatedFrom:{...ref('machine-c','absent'),title:'Missing remote',prompt:'SECRET'}})
 thread('unknown-2',{delegatedFrom:ref('machine-c','absent')})
 thread('bad',{delegatedFrom:ref('../bad','parent')});thread('incomplete',{delegatedFrom:{environmentId:'machine-b'}})
 thread('a',{delegatedFrom:ref('machine-a','b')});thread('b',{delegatedFrom:ref('machine-a','a')})
 for(let i=0;i<3;i++) {
  const id=`retry-${i}`;db.prepare(`INSERT INTO ${PREFIX}subagents(subagent_id,thread_id,payload_json) VALUES(?,?,?)`).run(id,'root',JSON.stringify({id,threadId:'root',runId:'rr',providerThreadId:`provider-${i}`,status:i===2?'completed':'running',updatedAt:iso(now-3000+i*1000),remoteChild:{...ref('machine-b','parent'),label:'B',result:'SECRET'}}))
  db.prepare(`INSERT INTO ${PREFIX}turn_items(thread_id,run_id,node_id,provider_thread_id,type,status,updated_at,ordinal,payload_json) VALUES(?,?,?,?,?,?,?,?,?)`).run('root','rr',id,`provider-${i}`,'command_execution','completed',iso(now-1000),i,JSON.stringify({input:'synthetic command',title:'Synthetic'}))
  db.prepare(`INSERT INTO ${PREFIX}provider_turns(thread_id,provider_thread_id,ordinal,started_at,payload_json) VALUES(?,?,?,?,?)`).run('root',`provider-${i}`,i,iso(now-2000),JSON.stringify({providerThreadId:`provider-${i}`,tokenUsage:{usedTokens:10+i,maxTokens:100}}))
 }
 const bounds=[0,36,59.9,60,392,599,600,1620,3600,17400,35999,36000,104400,1036800]
 for(const [i,sec] of bounds.entries()) {
  const id=`timing-${i}`;thread(id)
  const start=now-2*86400000-sec*1000,end=start+sec*1000
  db.prepare(`INSERT INTO ${PREFIX}runs(run_id,thread_id,ordinal,requested_at,completed_at,payload_json) VALUES(?,?,?,?,?,?)`).run(id,id,1,iso(start),iso(end),JSON.stringify({id,threadId:id,status:'completed',startedAt:iso(start),completedAt:iso(end)}))
  assert.ok(span(sec).length<=5)
 }
 thread('ongoing');db.prepare(`INSERT INTO ${PREFIX}runs(run_id,thread_id,ordinal,requested_at,payload_json) VALUES(?,?,?,?,?)`).run('ongoing','ongoing',1,iso(now-392000),JSON.stringify({id:'ongoing',threadId:'ongoing',status:'running',startedAt:iso(now-392000)}))
 db.close();const before=readFileSync(path)
 const expected=new Source(path).snapshot(now)
 // A path under a temporary directory would make the recording differ run to run.
 expected.path='synthetic:cross'
 const actual=JSON.parse(run(binary,['--db',path,'--now-ms',String(now),'--dump-snapshot']))
 assert.deepEqual(normalized(actual,true),normalized(expected,true))
 assert.deepEqual(readFileSync(path),before);assert.ok(!JSON.stringify(actual).includes('SECRET'))
 // The database and the machine id beside it travel as one tree of content-addressed assets.
 const recordedInput=recording?recordTree(dir):''
 const recordedGrid:any[]=[]
 const recordedFocused:any[]=[]
 const snapshot=join(dir,'snapshot.json');writeFileSync(snapshot,JSON.stringify(expected))
 const report=JSON.parse(run('node',[...node,'checks/scripts/grid-parity.ts',snapshot,join(dir,'grid.json')]))
 let focused=0
 for(const agent of expected.agents) {
  const args=['--snapshot',snapshot,'--grid','160x44','--actions',`pick:${agent.id}`]
  const ts=JSON.parse(run('node',[...node,'checks/scripts/grid-reference.ts',...args,'--json']))
  const rs=JSON.parse(run(binary,[...args,'--frame-json']))
  for(const field of ['ch','fg','bg','at']) {const diff=ts[field].flatMap((v:number,i:number)=>v!==rs[field][i]?[{x:i%160,y:Math.floor(i/160),expected:v,actual:rs[field][i],ts:ts.text.split('\n')[Math.floor(i/160)],rs:rs.text.split('\n')[Math.floor(i/160)]}]:[]);assert.equal(diff.length,0,`${agent.id}: ${field} ${JSON.stringify(diff.slice(0,4))}`)}
  if(recording)recordedFocused.push({id:agent.id,ch:numbersDigest(ts.ch),fg:numbersDigest(ts.fg),bg:numbersDigest(ts.bg),at:numbersDigest(ts.at)})
  focused++
 }
 if(recording){
  // The full grid pass over the same snapshot: every state's four planes as a digest.
  const grids=['233x55','237x57','160x44','95x35','71x37','60x30']
  const actions=['','down','end','home,space','pick:earlier:demo-webhooks','pick:earlier:demo-webhooks,enter','a','a,a','pick:demo-billing','pick:demo-checkout','pick:demo-remote','pagedown','?']
  for(const grid of grids)for(const steps of actions){
   const ts=JSON.parse(run('node',[...node,'checks/scripts/grid-reference.ts','--snapshot',snapshot,'--grid',grid,'--actions',steps,'--json']))
   recordedGrid.push({grid,actions:steps,ch:numbersDigest(ts.ch),fg:numbersDigest(ts.fg),bg:numbersDigest(ts.bg),at:numbersDigest(ts.at)})
  }
  recordCase({kind:'cross',now,input:recordedInput,snapshot:normalized(expected,true),render:expected,grid:recordedGrid,focused:recordedFocused})
 }
 console.log(JSON.stringify({snapshots:1,agents:actual.agents.length,gridStates:report.states,focusedStates:focused,textDifferences:report.textDifferences,colorDifferences:report.colorDifferences,attributeDifferences:report.attributeDifferences,errors:0}))
} finally {rmSync(dir,{recursive:true,force:true})}
