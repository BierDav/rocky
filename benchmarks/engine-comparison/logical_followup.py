#!/usr/bin/env python3
import pathlib,subprocess,json,re,shutil,time,os,signal
root=pathlib.Path(__file__).resolve().parent;out=root/'results';binary=root/'target/release/rocky-engine-bench'
with (out/'logical-followup.jsonl').open('a') as f:
 for repeat in range(3):
  for engine,cap in [('sqlite','trigger'),('sqlite','explicit'),('wal','cdc'),('wal','explicit'),('mvcc','cdc'),('mvcc','explicit')]:
   label=f'logical-payload-{engine}-{cap}-r{repeat}';path=out/'databases'/label
   cmd=['/usr/bin/time','-l',str(binary),'run',engine,cap,'independent','4','1','100','1',str(path)]
   t=time.time();p=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
   try:
    stdout,stderr=p.communicate(timeout=120)
    r={'label':label,'repeat':repeat,'profile':'logical-payload','command':cmd,'exit_code':p.returncode,'wall_seconds':time.time()-t,'stdout':stdout,'stderr':stderr}
    if p.returncode==0:r.update(json.loads(stdout))
    for key,pattern in [('peak_rss_bytes',r'(\d+)\s+maximum resident set size'),('user_cpu_s',r'([\d.]+) user'),('system_cpu_s',r'([\d.]+) sys')]:
     m=re.search(pattern,stderr)
     if m:r[key]=float(m[1])
   except subprocess.TimeoutExpired:
    os.killpg(p.pid,signal.SIGKILL);stdout,stderr=p.communicate();r={'label':label,'repeat':repeat,'exit_code':'timeout','stdout':stdout,'stderr':stderr}
   f.write(json.dumps(r)+'\n');f.flush();print(label,r['exit_code'],round(r.get('committed_tx_s',0)),r.get('foreground_drained'),flush=True);shutil.rmtree(path,ignore_errors=True)
