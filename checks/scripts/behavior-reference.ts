// Controlled-time oracle for the actual pinned App.key host and original draw path.
// No App.run(), terminal renderer, live database, real theme file, or focus connection.
import {readFileSync} from 'node:fs'
import {CRTRollingBarEffect,OptimizedBuffer,ResourceContext} from '../../oracle/node_modules/@opentui/core/index.node.js'
import {App,frameRate,flush} from '../../oracle/src/app.ts'
import {Source,type Snapshot} from '../../oracle/src/source.ts'
import {ThemeWatcher,derivePalette} from '../../oracle/src/theme.ts'
import {THEMES} from '../../oracle/test/themes.ts'
import {draw,pick} from '../../oracle/src/view.ts'
import {demoUsage} from '../../oracle/src/demo.ts'
import {semantic} from './oracle.ts'

const args=process.argv.slice(2)
const option=(name:string)=>{const i=args.indexOf(name);if(i<0||!args[i+1])throw Error(`missing ${name}`);return args[i+1]}
let current:Snapshot=JSON.parse(readFileSync(option('--snapshot'),'utf8'))
const spec=JSON.parse(readFileSync(option('--behavior-script'),'utf8'))
const epoch=current.at
let now=epoch
Date.now=()=>now
class Synthetic extends Source { constructor(){super('synthetic:behavior-oracle')} snapshot(){return current} }
// ThemeWatcher's normal constructor polls. Disable that read for this pure oracle.
const themePoll=ThemeWatcher.prototype.poll
ThemeWatcher.prototype.poll=()=>false
const usage=spec.usage??demoUsage(epoch,epoch)
const app=new App(new Synthetic(),2,{demo:true,calm:spec.calm??false,usage:{poll:()=>usage}})
ThemeWatcher.prototype.poll=themePoll
app.theme.palette=derivePalette(THEMES.tokyo)
app.theme.poll=()=>false
app.st.follow=false
app.st.bootAt=spec.bootAt??epoch
const canvas=app.canvas
canvas.resize(spec.width??233,spec.height??55)
const sorted=(m:Map<string,number>)=>[...m].sort(([a],[b])=>a<b?-1:a>b?1:0)
const keyName:Record<string,string>={space:'space',enter:'return'}
const sequence:Record<string,string>={space:' ',enter:'\r'}
const frames:any[]=[]
let reduced=false,lastEffect=epoch
const rolling=new CRTRollingBarEffect(0.35,0.12,0.18,0.4)
const effectPlanes={width:canvas.w,height:canvas.h,fg:new Uint16Array(canvas.w*canvas.h*4),bg:new Uint16Array(canvas.w*canvas.h*4)}
const effectOwner=new ResourceContext({objectCapacity:16,renderCellsMax:canvas.w*canvas.h*2})
const effectBuffer=OptimizedBuffer.create(canvas.w,canvas.h,'wcwidth',{id:'synthetic-crt-oracle',owner:effectOwner})
for(const [index,event] of [{ms:0},...spec.events].entries()){
 now=epoch+(event.ms??0)
 if(typeof event.reduced==='boolean')reduced=event.reduced
 if(event.snapshot){current=event.snapshot;if(!app.st.paused)app.refresh()}
 if(event.pick)pick(app.st,event.pick,now)
 if(event.key)app.key({name:keyName[event.key]??event.key,sequence:sequence[event.key]??event.key} as any)
 const manualCalm=app.st.calm
 if(reduced)app.st.calm=true
 app.frame=draw(canvas,app.st,app.snap,usage,app.theme.palette,now)
 const fps=frameRate(app.frame,app.st)
 let renderedFg=[...canvas.fg],renderedBg=[...canvas.bg]
 if(app.st.fx&&!app.st.calm){
  const dt=Math.max(0,now-lastEffect)
  rolling.apply({withBuffers:(callback:any)=>callback(effectPlanes)} as any,dt)
  flush(canvas,effectBuffer,0,0)
  ;(app as any).fx(effectBuffer,dt)
  effectBuffer.withBuffers(({fg,bg})=>{const rgb=(a:Uint16Array,i:number)=>((a[i]&255)<<16)|((a[i+1]&255)<<8)|(a[i+2]&255);renderedFg=Array.from({length:canvas.w*canvas.h},(_,i)=>rgb(fg,i*4));renderedBg=Array.from({length:canvas.w*canvas.h},(_,i)=>rgb(bg,i*4))})
 }
 lastEffect=now
 app.st.calm=manualCalm
 frames.push({index,now:app.st.lastFrame,semantic:semantic(app.st,app.frame),calm:app.st.calm,paused:app.st.paused,help:app.st.help,fx:app.st.fx,follow:app.st.follow,
  windowIndex:app.st.windowIndex,detailScroll:app.st.detailScroll,helpScroll:app.st.helpScroll,notice:app.st.notice,picked:app.st.picked,
  firstSeen:sorted(app.st.firstSeen),shimmer:sorted(app.st.shimmer),tween:sorted(app.st.tween),acked:[...app.st.acked].sort(),waits:sorted(app.st.waits),frozenAt:app.st.frozenAt,fps,fxPosition:(rolling as any).position,
  ch:[...canvas.ch],fg:renderedFg,bg:renderedBg,attr:[...canvas.at]})
}
effectBuffer.destroy();effectOwner.destroy()
console.log(JSON.stringify({frames}))
