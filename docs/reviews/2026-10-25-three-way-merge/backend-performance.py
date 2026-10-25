from ui import *
import re,statistics
results=[]
def measured(command,**args):
    t=time.monotonic();r=invoke(command,**args);duration=time.monotonic()-t
    if not r['ok']:raise RuntimeError(r)
    return r['value'],duration
for count in [100,250,500,1000,2000]:
    d=C/f'perf-{count}';d.mkdir(exist_ok=True)
    base=[];local=[];remote=[]
    for i in range(count):
        for v,label in [(base,'base'),(local,'local'),(remote,'remote')]:v.extend([f'{label} value {i}\n',*[f'context {i}-{j}\n'for j in range(6)]])
    for side,lines in [('base',base),('local',local),('remote',remote)]:(d/(side+'.txt')).write_text(''.join(lines))
    req={'kind':'sources',**{s:{'type':'local','path':str(d/(s+'.txt'))}for s in ['base','local','remote']},'output':str(d/'result.txt')}
    info,opened=measured('open_merge',request=req);id=info['id'];blocks=[x['block']for x in info['conflicts']];assert len(blocks)==count,(count,len(blocks))
    _,fetch=measured('merge_result_rows',id=id,start=0,count=200,theme='light')
    resolve=[];read=[]
    for b in blocks[:3]:
        _,t=measured('merge_resolve',id=id,block=b,choice='local');resolve.append(t)
        _,t=measured('merge_info',id=id);read.append(t)
    item={'conflicts':count,'inputLines':len(base),'bytesPerInput':(d/'base.txt').stat().st_size,'openSeconds':opened,'rowsSeconds':fetch,'resolveSeconds':resolve,'infoSeconds':read}
    results.append(item);event('backend-performance',item);(ROOT/'backend-performance.json').write_text(json.dumps(results,indent=2)+'\n');invoke('close_merge',id=id)
event('backend-performance-complete',results)
