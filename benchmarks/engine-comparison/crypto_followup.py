#!/usr/bin/env python3
import subprocess,pathlib,json,re,shutil
root=pathlib.Path(__file__).resolve().parent;out=root/'results'
with (out/'crypto-logical.jsonl').open('a') as f:
 for repeat in range(3):
  for engine in ['sqlite','wal','mvcc']:
   path=out/'databases'/f'crypto-logical-{engine}-{repeat}'
   cmd=['/usr/bin/time','-l',str(root/'target/release/rocky-engine-bench'),'run',engine,'explicit','draft','1','4','100','1',str(path),'crypto']
   p=subprocess.run(cmd,capture_output=True,text=True,timeout=90)
   r={'label':f'crypto-logical-{engine}-{repeat}','repeat':repeat,'command':cmd,'exit_code':p.returncode,'stderr':p.stderr,'stdout':p.stdout}
   if p.returncode==0:r.update(json.loads(p.stdout))
   for key,pat in [('peak_rss_bytes',r'(\d+)\s+maximum resident set size'),('user_cpu_s',r'([\d.]+) user'),('system_cpu_s',r'([\d.]+) sys')]:
    m=re.search(pat,p.stderr)
    if m:r[key]=float(m[1])
   f.write(json.dumps(r)+'\n');f.flush();print(engine,p.returncode,r.get('committed_tx_s'),flush=True);shutil.rmtree(path,ignore_errors=True)
