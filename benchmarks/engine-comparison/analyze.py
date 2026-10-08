#!/usr/bin/env python3
import json,pathlib,statistics,collections,csv
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
root=pathlib.Path(__file__).resolve().parent;out=root/'results';charts=root/'charts';charts.mkdir(exist_ok=True)
def load(name):return [json.loads(l) for l in (out/name).read_text().splitlines()] if (out/name).exists() else []
metrics=['committed_tx_s','logical_mutations_s','p50_ms','p95_ms','p99_ms','retries','reader_p95_ms','reader_errors','foreground_drained','remaining_drain_tx_s','remaining_drain_s','peak_rss_bytes','user_cpu_s','system_cpu_s','capture_lag_p50_ms','capture_lag_p95_ms','capture_lag_p99_ms','capture_lag_samples','capture_lag_missing']
rows=load('measurements.jsonl');focused=load('focused.jsonl')
def summarize(rows,profile):
 groups=collections.defaultdict(list)
 for r in rows:
  if r['exit_code']!=0:continue
  key=tuple(r[k] for k in ['engine','capture','work','writers','batch','files'])+(r.get('synchronous','FULL'),r.get('memory',False))
  groups[key].append(r)
 summary=[]
 for key,rs in groups.items():
  d=dict(zip(['engine','capture','work','writers','batch','files','sync','memory'],key));d['profile']=profile;d['runs']=len(rs)
  for m in metrics:
   vs=[r[m] for r in rs if m in r]
   if vs:d[m]=statistics.median(vs);d[m+'_min']=min(vs);d[m+'_max']=max(vs)
  sizes=[sum(r['storage'].values()) for r in rs];d['total_storage_bytes']=statistics.median(sizes)
  d['retry_per_commit']=d['retries']/(rs[0]['writers']*rs[0]['txs_per_writer'])
  d['cpu_seconds_per_commit']=statistics.median((r.get('user_cpu_s',0)+r.get('system_cpu_s',0))/(r['writers']*r['txs_per_writer']) for r in rs)
  summary.append(d)
 return summary
consumer=load('consumer-followup.jsonl')
storage=load('storage-followup.jsonl')
lag=load('lag-followup.jsonl')
logical=load('logical-followup.jsonl')
crypto_logical=load('crypto-logical.jsonl')
summary=summarize(rows,'matrix')+summarize(focused,'focused')+summarize(consumer,'reserved-consumer')+summarize(storage,'retained-handle')+summarize(lag,'lag-consumer')+summarize(logical,'logical-payload')+summarize(crypto_logical,'crypto-logical')
(out/'summary.json').write_text(json.dumps(summary,indent=2))
keys=list(dict.fromkeys(k for r in summary for k in r))
with (out/'summary.csv').open('w') as f:
 w=csv.DictWriter(f,keys);w.writeheader();w.writerows(summary)
variants=[('sqlite','trigger'),('wal','cdc'),('mvcc','cdc'),('sqlite','explicit'),('wal','explicit'),('mvcc','explicit')]
labels=['SQLite triggers','Turso WAL CDC','Turso MVCC CDC','SQLite explicit','Turso WAL explicit','Turso MVCC explicit']
colors=['#4d78a8','#f0a34a','#cc6262','#64a58d','#9c82bc','#38786d']
cases=[('draft',1,1,1),('independent',4,1,1),('contended',4,1,1),('replication',4,16,1),('authority',1,4,1),('mixed',4,4,1),('catchup',4,4,1),('independent',4,4,4),('independent',8,4,1),('independent',2,16,1)]
def find(profile,case,variant,sync='FULL',memory=False):
 work,w,b,files=case
 return next((s for s in summary if s['profile']==profile and (s['work'],s['writers'],s['batch'],s['files'])==(work,w,b,files) and (s['engine'],s['capture'])==variant and s['sync']==sync and s['memory']==memory),None)
for metric,title,file in [('committed_tx_s','Committed transactions/s (median; range across runs)','throughput'),('p99_ms','Transaction p99 milliseconds (median; range across runs)','p99')]:
 fig,axs=plt.subplots(2,5,figsize=(20,9));
 for ax,case in zip(axs.flat,cases):
  ds=[find('matrix',case,v) for v in variants];vals=[d[metric] if d else float('nan') for d in ds];ax.bar(range(6),vals,color=colors)
  for i,d in enumerate(ds):
   if d:ax.errorbar(i,d[metric],yerr=[[d[metric]-d[metric+'_min']],[d[metric+'_max']-d[metric]]],fmt='none',ecolor='black',capsize=2)
  ax.set_title(f'{case[0]}: {case[1]} writers / batch {case[2]} / {case[3]} files');ax.set_xticks(range(6),labels,rotation=65,ha='right',fontsize=8);ax.grid(axis='y',alpha=.2);ax.set_axisbelow(True)
 fig.suptitle(title+'\nFULL + fullfsync; warm small dataset; capture consumed');fig.tight_layout();fig.savefig(charts/f'{file}.png',dpi=160);fig.savefig(charts/f'{file}.svg');plt.close(fig)
fig,axs=plt.subplots(1,3,figsize=(16,5))
for ax,metric,title in zip(axs,['reader_p95_ms','remaining_drain_tx_s','peak_rss_bytes'],['Observer/consumer p95 (ms)','Residual catch-up transactions/s','Peak RSS (MiB)']):
 case=('mixed',4,4,1) if metric=='reader_p95_ms' else ('catchup',4,4,1)
 ds=[(find('reserved-consumer',case,v) if case[0]=='catchup' else None) or find('focused',case,v) or find('matrix',case,v) for v in variants];vals=[d.get(metric,0)/(1024**2 if metric=='peak_rss_bytes' else 1) if d else 0 for d in ds];ax.bar(range(6),vals,color=colors);ax.set_title(title);ax.set_xticks(range(6),labels,rotation=65,ha='right',fontsize=8)
fig.tight_layout();fig.savefig(charts/'consumer-memory.png',dpi=160);plt.close(fig)
lines=['# Measurements','', 'Medians of repeated runs. Full ranges, CPU, memory, storage and consumer metrics are in summary.csv. Failed cases are excluded and listed below.','', '| Work / writers / batch / files | Adapter | runs | tx/s | mutations/s | p50 / p95 / p99 ms | retries/commit | total MiB |','|---|---|---:|---:|---:|---|---:|---:|']
for s in summary:
 if s['profile']!='matrix':continue
 lines.append(f"| {s['work']} / {s['writers']} / {s['batch']} / {s['files']} | {s['engine']} {s['capture']} | {s['runs']} | {s['committed_tx_s']:.1f} | {s['logical_mutations_s']:.1f} | {s['p50_ms']:.2f} / {s['p95_ms']:.2f} / {s['p99_ms']:.2f} | {s['retry_per_commit']:.2f} | {s['total_storage_bytes']/1024**2:.2f} |")
lines+=['','## Failures','']
for r in rows+focused+consumer+storage+lag+logical+crypto_logical:
 if r['exit_code']!=0:lines.append(f"- {r['label']}: {r['exit_code']}; {r.get('stderr','')[:180]}")
(out/'MEASUREMENTS.md').write_text('\n'.join(lines)+'\n')
print('summary groups',len(summary),'matrix records',len(rows),'focused records',len(focused))

fig,axs=plt.subplots(1,3,figsize=(17,6))
for ax,(case,sync,memory,title) in zip(axs,[(('independent',4,1,1),'FULL',False,'Durable disk: FULL + fullfsync'),(('independent',4,1,1),'NORMAL',False,'Disk: weaker NORMAL sync'),(('draft',1,1,1),'FULL',True,'Memory: no disk persistence')]):
 ds=[find('focused',case,v,sync,memory) for v in variants]
 ax.bar(range(6),[d['committed_tx_s'] if d else 0 for d in ds],color=colors);ax.set_title(title);ax.set_xticks(range(6),labels,rotation=65,ha='right',fontsize=8);ax.set_ylabel('Committed transactions/s');ax.grid(axis='y',alpha=.2)
fig.tight_layout();fig.savefig(charts/'persistence-profiles.png',dpi=160);plt.close(fig)

fig,axs=plt.subplots(1,2,figsize=(12,6))
ds=[find('retained-handle',('independent',4,1,1),v) for v in variants]
axs[0].bar(range(6),[d['total_storage_bytes']/1024**2 if d else 0 for d in ds],color=colors);axs[0].set_title('Database + WAL/log + shared memory (MiB)')
image_bytes=[]
for v in variants:
 rs=[r for r in storage if r.get('exit_code')==0 and (r['engine'],r['capture'])==v]
 image_bytes.append(statistics.median(sum(c['capture_image_bytes'] for c in r['capture_storage']) for r in rs)/1024 if rs else 0)
axs[1].bar(range(6),image_bytes,color=colors);axs[1].set_title('Retained capture image payload (KiB)')
for ax in axs:ax.set_xticks(range(6),labels,rotation=65,ha='right',fontsize=8)
fig.suptitle('Matched open-handle storage: 400 timed + 10 warm-up transactions; no steady-state claim');fig.tight_layout();fig.savefig(charts/'storage.png',dpi=160);plt.close(fig)

fig,ax=plt.subplots(figsize=(9,5))
ds=[find('lag-consumer',('catchup',4,4,1),v) for v in variants]
ax.bar(range(6),[d.get('capture_lag_p95_ms',0) if d else 0 for d in ds],color=colors)
ax.set_title('Acknowledged commit to durable consumption: median run p95 (ms)');ax.set_xticks(range(6),labels,rotation=50,ha='right',fontsize=9);ax.set_ylabel('Milliseconds');fig.tight_layout();fig.savefig(charts/'capture-lag.png',dpi=160);plt.close(fig)

fig,axs=plt.subplots(1,2,figsize=(12,6))
ds=[find('logical-payload',('independent',4,1,1),v) for v in variants]
for ax,metric,title in zip(axs,['committed_tx_s','p99_ms'],['Committed transactions/s','Transaction p99 milliseconds']):
 ax.bar(range(6),[d[metric] if d else 0 for d in ds],color=colors);ax.set_title(title);ax.set_xticks(range(6),labels,rotation=65,ha='right',fontsize=8)
fig.suptitle('Complete declared JSON operations; four writers, batch one; FULL durable disk');fig.tight_layout();fig.savefig(charts/'logical-payload.png',dpi=160);plt.close(fig)
