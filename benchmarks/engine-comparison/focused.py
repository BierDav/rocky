#!/usr/bin/env python3
"""Follow-up: longer samples, origin/payload decoding, invalidation, memory and labeled weaker sync."""
import subprocess,pathlib,json,time,os,signal,shutil,re
root=pathlib.Path(__file__).resolve().parent;out=root/'results';binary=root/'target/release/rocky-engine-bench'
variants=[('sqlite','trigger'),('sqlite','explicit'),('wal','cdc'),('wal','explicit'),('mvcc','cdc'),('mvcc','explicit')]
# Source differs from initial matrix; profile marks decoded CDC and revision observers.
cases=[('independent',4,1,100,1,'FULL',False),('mixed',4,4,100,1,'FULL',False),('catchup',4,4,100,1,'FULL',False),('independent',4,4,100,4,'FULL',False),('draft',1,1,100,1,'FULL',True),('independent',4,1,100,1,'NORMAL',False)]
with (out/'focused.jsonl').open('a') as f:
 for repeat in range(3):
  for work,w,b,n,parts,sync,memory in cases:
   for engine,cap in variants[repeat:]+variants[:repeat]:
    label=f'focused-{engine}-{cap}-{work}-w{w}-b{b}-p{parts}-{sync}-memory{memory}-r{repeat}'
    path='memory' if memory else str(out/'databases'/label)
    cmd=['/usr/bin/time','-l',str(binary),'run',engine,cap,work,str(w),str(b),str(n),str(parts),path]
    env=os.environ.copy();env['ROCKY_SYNC']=sync;t=time.time()
    p=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=env,start_new_session=True)
    try:
     stdout,stderr=p.communicate(timeout=120)
     record={'label':label,'repeat':repeat,'profile':'decoded-cdc-revision-observers','command':cmd,'exit_code':p.returncode,'wall_seconds':time.time()-t,'stderr':stderr,'stdout':stdout,'sync':sync,'memory':memory}
     if p.returncode==0:record.update(json.loads(stdout))
     for key,pattern in [('peak_rss_bytes',r'(\d+)\s+maximum resident set size'),('user_cpu_s',r'([\d.]+) user'),('system_cpu_s',r'([\d.]+) sys')]:
      m=re.search(pattern,stderr)
      if m: record[key]=float(m[1])
    except subprocess.TimeoutExpired:
     os.killpg(p.pid,signal.SIGKILL);stdout,stderr=p.communicate()
     record={'label':label,'repeat':repeat,'command':cmd,'exit_code':'timeout','stdout':stdout,'stderr':stderr}
    f.write(json.dumps(record)+'\n');f.flush();print(label,record['exit_code'],round(record.get('committed_tx_s',0)),flush=True)
    if not memory:shutil.rmtree(path,ignore_errors=True)
