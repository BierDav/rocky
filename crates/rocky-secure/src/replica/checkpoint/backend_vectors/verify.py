"""Independent fixture construction using Python's standard hashlib, not Rust SHA2."""
from pathlib import Path
import hashlib, struct, json
out=Path(__file__).resolve().parent
def commitment(label, item):
    prefix=label.encode('utf-8')
    return hashlib.sha256(struct.pack('<I',len(prefix))+prefix+item).digest()
def lanes(items):
    result=[0]*1024
    for label,item in items:
        seed=commitment(label,item)
        expanded=b''.join(commitment('rocky.checkpoint-lanes.v1',seed+bytes([i])) for i in range(64))
        result=[(a+b)%65536 for a,b in zip(result,struct.unpack('<1024H',expanded))]
    return struct.pack('<1024H',*result)
def put(name, data): assert (out/name).read_bytes()==data, name
rowlabel='rocky.checkpoint-row.v2'; creationlabel='rocky.checkpoint-creation.v1'; handlelabel='rocky.checkpoint-handle.v1'
live=(out/'materialized-live.cbor').read_bytes(); deleted=(out/'materialized-deleted.cbor').read_bytes()
row=(rowlabel,bytes([8])*32+live)
tomb=(rowlabel,bytes([16])*32+deleted)
creation=(creationlabel,bytes([6])*32+struct.pack('<Q',7))
handle=(handlelabel,bytes([9])*32+bytes([10])*32+bytes([11])*32)
for name,items in [('empty',[]),('row',[row]),('mixed',[row,creation,handle]),('twice',[row,row]),('handle',[handle])]:
    data=lanes(items);put('lt-'+name+'.bin',data);put('lt-'+name+'-digest.bin',commitment('rocky.checkpoint-set.v1',data))
# Twice really crosses u16 wrapping, rather than just exercising small additions.
a=struct.unpack('<1024H',lanes([row])); assert any(v*2>65535 for v in a)
states=[('empty',0,0,0,[],[]),('live',1,0,1,[row],[creation]),('deleted',1,1,1,[(rowlabel,bytes([8])*32+deleted)],[creation]),('removed',0,0,1,[],[creation]),('merged',2,1,1,[row,tomb],[creation])]
for name,r,t,c,rows,creations in states:
    state=struct.pack('<QQQ',r,t,c)+lanes(rows)+lanes(creations)
    put('sets-'+name+'.bin',state)
    put('sets-'+name+'-summaries.bin',commitment('rocky.checkpoint-set.v1',lanes(rows))+commitment('rocky.checkpoint-set.v1',lanes(creations)))
# Small, independent CBOR reader for these deterministic fixture values only.
def decode(data):
    def read(i):
        first=data[i];i+=1;major=first>>5;info=first&31
        if major==7: return {20:False,21:True,22:None}[info],i
        if info<24:n=info
        else:
            size={24:1,25:2,26:4,27:8}[info];n=int.from_bytes(data[i:i+size],'big');i+=size
        if major==0:return n,i
        if major==1:return -1-n,i
        if major in (2,3):return (data[i:i+n] if major==2 else data[i:i+n].decode()),i+n
        if major==4:
            values=[]
            for _ in range(n):v,i=read(i);values.append(v)
            return values,i
        if major==5:
            values={}
            for _ in range(n):k,i=read(i);v,i=read(i);values[k]=v
            return values,i
        raise ValueError(major)
    value,end=read(0);assert end==len(data);return value
signed=decode((out/'signed.cbor').read_bytes());statement=decode((out/'statement.cbor').read_bytes())
assert signed['statement']==statement
signing=decode((out/'signing.bin').read_bytes())
assert signing==['rocky.encrypted-checkpoint.v3',[statement,signed['node'],signed['credentials']]]
for field,items,count in [('rows',[row],1),('creations',[creation],1),('handles',[handle],1)]:
    assert bytes(statement[field]['digest'])==commitment('rocky.checkpoint-set.v1',lanes(items))
    assert statement[field]['count']==count
assert bytes(statement['state']['Keyed']['key_epoch']['secret_hash'])==commitment('rocky.key-epoch.v1',bytes([3])*32)
assert bytes(signed['signature'])==(out/'signature.bin').read_bytes()
assert (out/'checkpoint-id.bin').read_bytes()==commitment('rocky.encrypted-checkpoint.id.v3',(out/'signed.cbor').read_bytes())
hashes={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(out.iterdir()) if p.is_file()}
print('Independently checked roots, key epoch, signing envelope, checkpoint ID; frozen',len(hashes),'files')
print(json.dumps(hashes,indent=2))
