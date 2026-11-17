// Export only the authored made-up scenario, not application or account state.
// Run when the baseline demo's scenario changes; no Node dependency at native runtime.
import {readFileSync,writeFileSync} from 'node:fs'
import {resolve,dirname} from 'node:path'
import {fileURLToPath} from 'node:url'
import {runInNewContext} from 'node:vm'
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..')
const source=readFileSync(resolve(root,'../oracle/src/demo.ts'),'utf8')
const models=Object.fromEntries([...source.matchAll(/^const (OPUS|SONNET|HAIKU|GPT|GROK|GROK_FAST) = "([^"]+)"/gm)].map(m=>[m[1],m[2]]))
const literal=source.split('const SCENARIO: Role[] = ')[1]?.split('// ── Building the database')[0]?.trim()
const projectLiteral=source.split('const PROJECTS = ')[1]?.split('const OPUS = ')[0]?.trim()
if(!literal||!projectLiteral||Object.keys(models).length!==6)throw new Error('Unsupported demo source: inspect the scenario export before updating it')
const roles=runInNewContext('('+literal+')',models,{timeout:1000})
const projects=runInNewContext('('+projectLiteral+')',{}, {timeout:1000})
writeFileSync(resolve(root,'fixtures/scenario.json'),JSON.stringify({projects,roles},null,2)+'\n')
console.log('Exported authored synthetic roles and projects; no database read')
