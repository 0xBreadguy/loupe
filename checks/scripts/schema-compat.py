#!/usr/bin/env python3
"""Headless synthetic schema compatibility check. No reader modifications.

python3 checks/scripts/schema-compat.py [--version main-096|older-074|replayed-083|legacy-054|all]
Add --negative parity|message|incremental|self-check to prove an assertion fails.
All generated files stay in a new checks/generated/schema-compat/run-* directory.
"""
import argparse, json, os, select, sqlite3, subprocess, tempfile
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--version', default='all', choices=['all','main-096','older-074','replayed-083','legacy-054'])
p.add_argument('--negative', choices=['parity','message','incremental','self-check'])
a = p.parse_args()
(ROOT/'checks/generated/schema-compat').mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix='run-', dir=ROOT/'checks/generated/schema-compat'))
home = work/'empty-t3-home'; home.mkdir()
env = {'PATH':os.environ['PATH'], 'T3CODE_HOME':str(home), 'TZ':'UTC', 'NO_COLOR':'1'}
node = ['node','--experimental-ffi','--disable-warning=ExperimentalWarning']
binary = Path(os.environ.get('T3_NATIVE_BINARY', ROOT/'target/release/t3-agents-core')).resolve()
now = '1794484800000'
def run(args):
    r = subprocess.run([str(x) for x in args], cwd=ROOT, env=env, text=True, capture_output=True, timeout=90)
    if r.returncode: raise RuntimeError(r.stderr)
    return r.stdout
run(node + ['checks/scripts/scale-db.ts','--tier','s','--db',work/'seed.sqlite'])
source = sqlite3.connect(work/'seed.sqlite')
def normalize(x, snapshot=False):
    if isinstance(x, list): return [normalize(v) for v in x]
    if isinstance(x, dict): return {k:normalize(v, k=='snapshot') for k,v in x.items() if v is not None and not (snapshot and k=='path')}
    return x
def full(db):
    js = "import {Source} from './oracle/src/source.ts'; try {console.log(JSON.stringify({snapshot:new Source(process.argv[1]).snapshot(Number(process.argv[2]))}))} catch(e) {console.log(JSON.stringify({error:e.message}))}"
    ts = json.loads(run(node + ['--input-type=module','-e',js,str(db),now]))
    r = subprocess.run([str(binary),'--db',str(db),'--now-ms',now,'--dump-snapshot'], env=env, text=True, capture_output=True, timeout=30)
    assert r.returncode == 0, r.stderr
    # --dump-snapshot suppresses source errors into an empty at=0 snapshot.
    # The existing benchmark front door exposes source::snapshot's Display message.
    probe = subprocess.run([str(binary),'--bench-source','1','--db',str(db),'--now-ms',now], env=env, text=True, capture_output=True, timeout=30)
    details = json.loads(probe.stdout)
    assert 'first_error' in details or (probe.returncode == 0 and details['errors'] == 0), details
    rust = {'error':details['first_error']} if 'first_error' in details else {'snapshot':json.loads(r.stdout)}
    return normalize(ts), normalize(rust)
def compare(db):
    ts, rust = full(db)
    if a.negative == 'parity' and 'snapshot' in rust: rust['snapshot']['agents'][0]['title'] = 'injected difference'
    if a.negative == 'message' and 'error' in rust: rust['error'] += ' injected difference'
    assert ts == rust, {'db':str(db), 'ts':ts if 'error' in ts else 'snapshot', 'rust':rust if 'error' in rust else 'snapshot', 'finding':'full readers differ'}
    return ts
report = {'work':str(work),'versions':{}}
versions = ['main-096','older-074','replayed-083','legacy-054'] if a.version == 'all' else [a.version]
for version in versions:
    dbpath = work/(version+'.sqlite')
    db = sqlite3.connect(dbpath)
    fixture = ROOT/'checks/fixtures'/('t3-schema-'+version+'.sql')
    db.executescript(fixture.read_text())
    for (table,) in source.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetchall():
        if table == 'effect_sql_migrations': continue
        columns = [row[1] for row in db.execute(f'PRAGMA table_info({table})')]
        old = {row[1] for row in source.execute(f'PRAGMA table_info({table})')}
        columns = [c for c in columns if c in old]
        if not columns: continue
        names = ','.join(columns)
        rows = source.execute(f'SELECT {names} FROM {table}').fetchall()
        db.executemany(f'INSERT INTO {table}({names}) VALUES ({",".join("?" for _ in columns)})',rows)
    if version == 'replayed-083':
        db.execute("UPDATE orchestration_v2_projection_threads SET payload_json=json_set(payload_json,'$.titleRegeneration',json('{\"requestId\":\"stale-synthetic\"}'))")
        db.execute("UPDATE orchestration_v2_projection_turn_items SET payload_json=json_set(payload_json,'$.toolName','mcp__t3_code__delegate_task') WHERE type='dynamic_tool'")
        db.execute("UPDATE orchestration_events SET event_type='project.meta-updated',payload_json='{\"title\":\"renamed\"}' WHERE aggregate_kind='project'")
    db.commit()
    initial = compare(dbpath)
    if 'error' in initial:
        assert version == 'legacy-054', ('unexpected schema refusal', version, initial)
        report['versions'][version] = initial
        db.close()
        continue
    assert version != 'legacy-054', 'legacy schema unexpectedly accepted'
    assert len(initial['snapshot']['agents']) == 25
    proc = subprocess.Popen([str(binary),'--bench-reader-poll','--self-check','--db',str(dbpath),'--now-ms',now], env=env, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    polls = []
    def poll(label):
        proc.stdin.write('\n'); proc.stdin.flush()
        assert select.select([proc.stdout],[],[],30)[0], 'poll timeout'
        line = proc.stdout.readline()
        assert line, proc.stderr.read()
        result = json.loads(line)
        assert not result['mismatch'] and result['mismatches'] == 0, result
        polls.append({'write':label, **{k:result[k] for k in ['served','fallback_reason','agents','mismatch']}})
    prefix = 'orchestration_v2_projection_'
    tid = db.execute(f'SELECT thread_id FROM {prefix}threads LIMIT 1').fetchone()[0]
    def event():
        seq = db.execute('SELECT MAX(sequence)+1 FROM orchestration_events').fetchone()[0]
        ver = db.execute("SELECT MAX(stream_version)+1 FROM orchestration_events WHERE aggregate_kind='thread' AND stream_id=?",(tid,)).fetchone()[0]
        db.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,actor_kind,payload_json,metadata_json,application_event_version) VALUES (?,'thread',?,?,'thread.meta-updated','2026-11-12T12:00:00.000Z','server','{}','{}',2)",(f'compat-{seq}',tid,ver))
        db.execute(f"UPDATE {prefix}metadata SET last_sequence=? WHERE projection_name='thread-projections'",(seq,))
    try:
        poll('initial'); poll('idle')
        if a.negative == 'self-check':
            # Deliberately violate event attribution: change B but log only A.
            db.execute(f"UPDATE {prefix}threads SET payload_json=json_set(payload_json,'$.title','unlogged B') WHERE thread_id <> ?",(tid,))
            event(); db.commit(); poll('deliberately unlogged cross-thread write')
        for i in range(12):
            # Alternate thread metadata, run state and tool activity; event/cursor commit together.
            if i%3 == 0: db.execute(f"UPDATE {prefix}threads SET payload_json=json_set(payload_json,'$.title',?) WHERE thread_id=?",(f'compat title {i}',tid))
            elif i%3 == 1: db.execute(f"UPDATE {prefix}runs SET status=?,payload_json=json_set(payload_json,'$.status',?) WHERE thread_id=?",('running' if i%2 else 'completed',)*2+(tid,))
            else: db.execute(f"UPDATE {prefix}turn_items SET status=?,payload_json=json_set(payload_json,'$.status',?) WHERE thread_id=? AND type='command_execution'",('running' if i%2 else 'completed',)*2+(tid,))
            if a.negative == 'incremental':
                db.execute('INSERT INTO effect_sql_migrations(migration_id,name) VALUES (?,?)',(20000+i,'force full read'))
            event(); db.commit(); result = compare(dbpath)
            if i%3 == 0: assert any(x['title']==f'compat title {i}' for x in result['snapshot']['agents'])
            poll(f'event-write-{i}')
        if version == 'replayed-083':
            upgrade = json.loads((ROOT/'checks/fixtures/t3-upgrade-083-096.json').read_text())
            for stage in upgrade['stages']:
                with db:
                    for sql in stage['sql']: db.execute(sql)
                    db.execute('INSERT INTO effect_sql_migrations(migration_id,name) VALUES (?,?)',(stage['id'],stage['name']))
                assert compare(dbpath) == result, ('upgrade changed visible agents',stage['id'])
                poll(f"T3 migration {stage['id']}")
                assert polls[-1]['served']=='Full', polls[-1]
            assert db.execute('SELECT SUM(has_custom_title) FROM projection_projects').fetchone()[0] == 8
            assert db.execute("SELECT COUNT(*) FROM orchestration_v2_projection_turn_items WHERE type='dynamic_tool' AND inline_work_output=1").fetchone()[0] > 0
            assert db.execute("SELECT COUNT(*) FROM orchestration_v2_projection_threads WHERE json_type(payload_json,'$.titleRegeneration')='object'").fetchone()[0] == 0
        # Data-only migration: projection write and ledger entry, no event.
        db.execute(f"UPDATE {prefix}threads SET payload_json=json_set(payload_json,'$.title','data-only migration') WHERE thread_id=?",(tid,))
        db.execute("INSERT INTO effect_sql_migrations(migration_id,name) VALUES (10001,'SyntheticDataRepair')")
        db.commit(); compare(dbpath); poll('data-only migration'); poll('idle after migration')
        db.execute(f'ALTER TABLE {prefix}threads ADD COLUMN compat_probe TEXT')
        db.execute("INSERT INTO effect_sql_migrations(migration_id,name) VALUES (10002,'SyntheticDDL')")
        db.commit(); compare(dbpath); poll('DDL migration')
        db.execute(f"UPDATE {prefix}threads SET payload_json=json_set(payload_json,'$.title','after DDL') WHERE thread_id=?",(tid,))
        event(); db.commit(); compare(dbpath); poll('event after DDL')
        incremental = sum(x['served']=='Incremental' for x in polls)
        assert incremental == 13, polls
        assert polls[0]['fallback_reason']=='InitialBaseline', polls
        assert polls[1]['served']=='Skipped' and polls[-3]['served']=='Skipped', polls
        assert polls[-2]['fallback_reason']=='SchemaChanged' and polls[-4]['fallback_reason']=='LedgerChanged', polls
    finally:
        proc.stdin.close()
        try: proc.wait(timeout=5)
        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
    assert proc.returncode == 0, proc.stderr.read()
    db.close()
    report['versions'][version] = {'agents':len(initial['snapshot']['agents']), 'full_comparisons':16 + (13 if version=='replayed-083' else 0), 'polls':polls}
# Minimal, empty missing-table database. Both must report the exact same message.
missing = work/'missing.sqlite'; sqlite3.connect(missing).close()
report['missing_tables'] = compare(missing)
assert report['missing_tables'].get('error', '').startswith('Unsupported T3 schema; missing ')
(work/'report.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
