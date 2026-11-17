// Actual baseline visual oracle: fixed synthetic snapshot/time/theme/cell geometry.
import {readFileSync,writeFileSync} from 'node:fs'
import {Canvas} from '../../oracle/src/canvas.ts'
import {draw,initialState,observe} from '../../oracle/src/view.ts'
import {derivePalette} from '../../oracle/src/theme.ts'
import {THEMES} from '../../oracle/test/themes.ts'
import {demoUsage} from '../../oracle/src/demo.ts'
import {action} from './oracle.ts'
const args=process.argv.slice(2), get=(name:string,otherwise:string)=>args.includes(name)?args[args.indexOf(name)+1]:otherwise
const snap=JSON.parse(readFileSync(get('--snapshot','checks/fixtures/demo.json'),'utf8'))
const [w,h]=get('--grid','237x57').split('x').map(Number)
const state=initialState(2);state.calm=true;state.demo=true;state.follow=false;state.bootAt=0
observe(state,snap,snap.at)
const canvas=new Canvas(w,h), palette=derivePalette(THEMES.tokyo), usage=demoUsage(snap.at,snap.at)
let frame=draw(canvas,state,snap,usage,palette,snap.at)
if(args.includes('--actions')) for(const key of get('--actions','').split(',').filter(Boolean)){if(key==="?"||key==="h"){state.help=!state.help;state.helpScroll=0}else if((key==="enter"||key==="o")&&!frame.rows.find(r=>r.id===state.selected)?.earlier){const a=snap.agents.find(a=>a.id===state.selected);if(a){const thread=snap.agents.find(t=>t.id===a.thread_id)??a;state.notice={text:`Demo: would open ${thread.title} in T3 Code`,at:snap.at,error:false}}}else action(state,frame,snap,key);frame=draw(canvas,state,snap,usage,palette,snap.at)}
if(args.includes('--json')) console.log(JSON.stringify({w,h,text:canvas.toText(),selected:state.selected,list:frame.list,detail:frame.detail,detailMax:frame.detailMax,ch:[...canvas.ch],fg:[...canvas.fg],bg:[...canvas.bg],at:[...canvas.at]}))
else console.log(canvas.toText())
