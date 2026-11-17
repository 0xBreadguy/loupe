#!/usr/bin/env python3
"""Curate local measurement output; no builds, windows or measurements are started."""
import json
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
NAMES=['model-demo','model-large','gui-idle','gui-updates','startup','interaction']
reports={n:json.loads((ROOT/'measurements'/f'{n}.json').read_text()) for n in NAMES}
result={'verdict':'Keep exploring; adoption performance inconclusive.',
 'interpretation':['Model wait4 peak RSS includes possible launcher pre-exec residency and is excluded from memory comparisons.',
 'GUI memory uses sampled process-tree /proc PSS; GPU and compositor excluded from both sides.',
 'Update sampling covers replay tail plus idle, not all-update processing cost.',
 'Startup/interaction are instrumented omabox operational proxies, not first-present/input-to-present.'],
 'discarded_protocols':[{'name':'initial-gui-idle','rejected_samples':10,'reason':'Incorrect guard expected node instead of node-MainThread; terminal-only control also incorrectly required Node. Final protocol corrected and fully rerun.'}],
 'datasets':{}}
for name,r in reports.items():
 assert r['errors']==0,(name,r['errors'])
 keep={k:v for k,v in r.items() if k not in ['trials','oracle','working_tree']}
 trials=[]
 for t in r['trials']:
  row={'side':t['side'],'trial':t['trial']};m=t['result']
  if name.startswith('model'):
   row.update({k:v for k,v in m.items() if k not in ['row_ids','stderr']})
   row['peak_rss_scope']='launcher-inclusive accounting upper bound; not compared'
  elif name.startswith('gui'):
   row.update({k:v for k,v in m.items() if k!='samples'})
  else:
   row.update(errors=m['errors'],window=m.get('verified_window'))
   if name=='startup':row['settle_proxy_ms']=m['settle_proxy_ms']
   else:row['operations']=[{'key':op['key'],'errors':op['errors'],'settle_proxy_ms':op['settle_proxy_ms'],
         'state_verified':op['state']['equal'],'selected':op['state']['observed']['selected']} for op in m['operations']]
  trials.append(row)
 keep['trials']=trials
 result['datasets'][name]=keep
out=ROOT/'results'/'measurements.json';out.parent.mkdir(exist_ok=True);out.write_text(json.dumps(result,indent=2)+'\n')

def v(value,scale=1,unit='',digits=2):
 return f"{value['median']/scale:.{digits}f}{unit} ({value['min']/scale:.{digits}f}–{value['max']/scale:.{digits}f})"
lines=['| Measurement · median (range) | OpenTUI / TypeScript | Rune / Rust | Coverage |','| --- | --- | --- | --- |']
for name,label in [('model-demo','25-agent demo'),('model-large','256-agent large')]:
 r=reports[name]['summary']['processing']
 for field,title in [('parse_ms','parse'),('model_ms','tree')]:
  lines.append(f"| {label} {title}, ms/op | {v(r['typescript'][field],100,digits=3)} | {v(r['rust'][field],100,digits=3)} | 5 trials × 100 ops/side; no presentation |")
for name,label in [('gui-idle','Calm idle'),('gui-updates','Replay tail + idle')]:
 r=reports[name]['summary'];a=r['typescript_terminal_total'];b=r['rust_native']
 lines.append(f"| {label} PSS, MiB | {v(a['pss_kib'],1024)} | {v(b['pss_kib'],1024)} | 5 trials/side; terminal included |")
 lines.append(f"| {label} CPU, % one core | {v(a['cpu_percent_one_core'])} | {v(b['cpu_percent_one_core'])} | {'3' if name=='gui-idle' else '4'} s sampled after 1 s warmup |")
for name,label in [('startup','Launch-to-settle proxy, ms'),('interaction','Key-to-settle proxy, ms')]:
 r=reports[name]['summary']
 def p(side):
  q=r[side];return f"{q['median_ms']:.1f} ({q['min_ms']:.1f}–{q['max_ms']:.1f})"
 lines.append(f"| {label} | {p('typescript_terminal_total')} | {p('rust_native')} | {'5 launches/side' if name=='startup' else '20 state-verified keys/side'}; includes IPC/debounce |")
report=ROOT/'REPORT.md';text=report.read_text();start=text.index('<!-- measured-table -->');end=text.find('\n\nModel timings',start)
text=text[:start]+'<!-- measured-table -->\n\n'+'\n'.join(lines)+text[end:];report.write_text(text)
print(out)
print('\n'.join(lines))

# Standalone source-backed chart, also usable in T3's inline HTML renderer.
bars=[]
for group,(name,title) in enumerate([('gui-idle','Calm idle'),('gui-updates','Replay tail + idle')]):
 summary=reports[name]['summary']
 for side,label,color in [('typescript_terminal_total','Terminal + Node','--chart-1'),('rust_native','Rune + Rust','--chart-2')]:
  value=summary[side]['pss_kib'];y=35+group*100+(0 if side=='typescript_terminal_total' else 32)
  x=lambda n: 170+(n/1024)/180*410
  bars.append(f'<text x="0" y="{y+15}">{label}</text><rect x="170" y="{y}" width="{x(value["median"])-170:.2f}" height="22" fill="var({color})"/><path d="M{x(value["min"]):.2f} {y+11}H{x(value["max"]):.2f}" stroke="var(--foreground)" stroke-width="2"/><text x="{x(value["max"])+8:.2f}" y="{y+16}">{value["median"]/1024:.1f}</text>')
html='''<!doctype html><html><head><meta charset="utf-8"><style>body{margin:0;color:var(--foreground);font-family:var(--font-sans,system-ui)}svg{width:100%;height:270px}text{fill:var(--foreground);font-size:13px}p{font-size:13px;color:var(--muted-foreground);line-height:1.5}strong{font-weight:600}</style></head><body><strong>Sampled application memory · MiB PSS</strong><svg viewBox="0 0 700 270" role="img" aria-label="Median proportional resident memory of five trials per side, with range marks. Values are listed beside each bar.">'''
html+='<text x="170" y="22">Calm idle</text><text x="170" y="122">Replay tail + idle</text>'+''.join(bars)
html+='<path d="M170 215H580" stroke="var(--border)"/>'+''.join(f'<text x="{170+i*410/3:.1f}" y="238">{i*60}</text>' for i in range(4))+'</svg><p>Median and observed range, five alternating trials per side. Includes the standalone terminal on the baseline side. Excludes GPU allocations and compositor resources on both sides. The native slice has fewer features; this does not establish a framework or input-latency advantage.</p></body></html>'
(ROOT/'results'/'memory.html').write_text(html)
