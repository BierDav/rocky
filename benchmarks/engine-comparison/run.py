#!/usr/bin/env python3
"""Bounded release matrix. Every case gets a fresh file; errors remain raw evidence."""
import argparse,json,pathlib,subprocess,time,platform,re,shutil,os,signal
p=argparse.ArgumentParser();p.add_argument('--quick',action='store_true');p.add_argument('--output',default='results');a=p.parse_args()
root=pathlib.Path(__file__).resolve().parent;out=root/a.output;out.mkdir(parents=True,exist_ok=True)
binary=root/'target/release/rocky-engine-bench'
variants=[('sqlite','none'),('sqlite','trigger'),('sqlite','explicit'),('wal','none'),('wal','cdc'),('wal','explicit'),('mvcc','none'),('mvcc','cdc'),('mvcc','explicit')]
cases=[('draft',1,1,1),('independent',4,1,1),('contended',4,1,1),('replication',4,16,1),('authority',1,4,1),('mixed',4,4,1),('catchup',4,4,1),('independent',4,4,4),('independent',8,4,1),('independent',2,16,1)]
if a.quick: cases=cases[:2]
with (out/'measurements.jsonl').open('a') as f:
 for repeat in range(3):
  for work,w,b,parts in cases:
   # Rotate ordering to reduce thermal/order bias.
   for engine,cap in variants[repeat:]+variants[:repeat]:
    label=f'{engine}-{cap}-{work}-w{w}-b{b}-p{parts}-r{repeat}'
    path=out/'databases'/label
    cmd=['/usr/bin/time','-l',str(binary),'run',engine,cap,work,str(w),str(b),'40',str(parts),str(path)]
    t=time.time()
    try:
     p=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
     stdout,stderr=p.communicate(timeout=90)
     r=subprocess.CompletedProcess(cmd,p.returncode,stdout,stderr)
     record={'label':label,'repeat':repeat,'command':cmd,'exit_code':r.returncode,'wall_seconds':time.time()-t,'stderr':r.stderr,'stdout':r.stdout}
     for line in r.stdout.splitlines():
      try: record.update(json.loads(line))
      except json.JSONDecodeError: pass
     for key,pattern in [('peak_rss_bytes',r'(\d+)\s+maximum resident set size'),('user_cpu_s',r'([\d.]+) user'),('system_cpu_s',r'([\d.]+) sys')]:
      m=re.search(pattern,r.stderr)
      if m: record[key]=float(m[1])
    except subprocess.TimeoutExpired as e:
     os.killpg(p.pid,signal.SIGKILL);p.communicate()
     record={'label':label,'repeat':repeat,'command':cmd,'exit_code':'timeout','stdout':str(e.stdout),'stderr':str(e.stderr)}
    f.write(json.dumps(record)+'\n');f.flush()
    print(label,record['exit_code'],round(record.get('committed_tx_s',0)),flush=True)
    # Retain raw metrics and storage sizes, remove large reproducible database files.
    shutil.rmtree(path,ignore_errors=True)
# Separate equivalent crypto scenario.
with (out/'crypto.jsonl').open('a') as f:
 for repeat in range(3):
  for engine in ['sqlite','wal','mvcc']:
   path=out/'databases'/f'crypto-{engine}-{repeat}'
   cmd=['/usr/bin/time','-l',str(binary),'run',engine,'explicit','draft','1','4','100','1',str(path),'crypto']
   r=subprocess.run(cmd,capture_output=True,text=True,timeout=90)
   record={'engine':engine,'repeat':repeat,'command':cmd,'exit_code':r.returncode,'stderr':r.stderr,'stdout':r.stdout}
   if r.returncode==0:record.update(json.loads(r.stdout))
   f.write(json.dumps(record)+'\n');f.flush();shutil.rmtree(path,ignore_errors=True)
