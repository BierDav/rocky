#!/usr/bin/env python3
import subprocess,pathlib,json,tempfile,os,signal
root=pathlib.Path(__file__).resolve().parent;binary=root/'target/release/rocky-engine-bench'
with (root/'results/crash.jsonl').open('w') as out:
 for engine,cap in [('sqlite','trigger'),('sqlite','explicit'),('wal','cdc'),('mvcc','cdc'),('wal','explicit'),('mvcc','explicit')]:
  path=tempfile.mkdtemp(prefix='rocky-crash-')+'/db'
  p=subprocess.Popen([str(binary),'crash-write',engine,cap,path],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
  ack=p.stdout.readline().strip()
  if ack=='ACK':p.kill()
  stdout,stderr=p.communicate(timeout=90)
  r=subprocess.run([str(binary),'crash-read',engine,cap,path],capture_output=True,text=True,timeout=90)
  record={'engine':engine,'capture':cap,'ack':ack,'signal':p.returncode,'read_exit':r.returncode,'stdout':r.stdout,'stderr':stderr+r.stderr}
  out.write(json.dumps(record)+'\n');out.flush();print(record)
