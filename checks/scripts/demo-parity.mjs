// Full synthetic Demo→Source correctness gate. No rendering, service or real DB.
// The executable must expose the no-window --demo-snapshot mode from this experiment.
import assert from 'node:assert/strict'
import {createHash} from 'node:crypto'
import {readFileSync,writeFileSync,mkdirSync,mkdtempSync,rmSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {dirname,resolve,join} from 'node:path'
import {fileURLToPath} from 'node:url'
import {spawnSync} from 'node:child_process'
import {buildDemo} from '../../oracle/src/demo.ts'
import {Source} from '../../oracle/src/source.ts'
// Opt-in: with T3_RECORD set, export-demo.ts turns this run into the recorded fixture.
import {enabled as recording, recordCase} from './record.ts'
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..')
const binary=resolve(process.argv[2]??process.env.T3_NATIVE_BINARY??join(root,'..','target/release/t3-agents-core'))
const output=resolve(process.argv[3]??join(root,'generated/demo-parity.json'))
const executable=readFileSync(binary)
// Fail before invoking stale binaries: older prototypes ignore this unknown flag
// and may launch a window. The app-owned option is a unique compiled string.
if(!executable.includes(Buffer.from('--demo-snapshot')))throw new Error('Native binary lacks the no-window demo CLI; build the current revision first')
const epoch=1794484800000
const offsets=[-1,0,3000,59999,60000,120000,179999,180000,359999,360000,539999,540000,900000,1199999,1200000,1230000,2400000,3600000]
const directory=mkdtempSync(join(tmpdir(),'t3-rune-demo-parity-'))
// Rust serializes optional DTO fields as null, while JS omits undefined fields.
// Those absent object fields are equivalent; array order and null elements stay strict.
function normalized(value,root=false){
 if(Array.isArray(value))return value.map(v=>normalized(v))
 if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).filter(([k,v])=>v!==null&&!(root&&k==='path')).map(([k,v])=>[k,normalized(v)]))
 return value
}
const cases=[]
try{
 for(const offset of offsets){
  const path=join(directory,'synthetic.sqlite')
  buildDemo(path,epoch+offset,epoch)
  const expected=new Source(path).snapshot(epoch+offset)
  const result=spawnSync(binary,['--demo-snapshot',String(epoch+offset),'--demo-epoch',String(epoch)],{encoding:'utf8',timeout:30000,maxBuffer:32*1024*1024})
  assert.equal(result.status,0,`${offset}ms: ${result.error??result.stderr}`)
  const actual=JSON.parse(result.stdout)
  assert.deepEqual(normalized(actual,true),normalized(expected,true),`${offset}ms full snapshot differs`)
  if(recording)recordCase({kind:'demo',offset,now:epoch+offset,epoch,expected:normalized(expected,true)})
  cases.push({offset_ms:offset,agents:actual.agents.length,events:actual.agents.reduce((n,a)=>n+a.events.length,0),queued:actual.agents.filter(a=>a.status==='queued').map(a=>a.id),errors:0})
 }
}finally{rmSync(directory,{recursive:true,force:true})}
const report={version:1,binary_sha256:createHash('sha256').update(executable).digest('hex'),scenario_sha256:createHash('sha256').update(readFileSync(join(root,'fixtures/scenario.json'))).digest('hex'),operations:cases.length,errors:0,epoch,cases,normalization:'Only optional object null/absence and root snapshot path; all values, array ordering, timestamps, events, plans, histories, requests and item metadata remain exact'}
mkdirSync(dirname(output),{recursive:true});writeFileSync(output,JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify(report))
