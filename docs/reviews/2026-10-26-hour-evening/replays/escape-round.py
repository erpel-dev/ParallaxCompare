from manual import *
for side in ['left','right']:
 for name in ['a,b.txt','a.txt','b.txt','c.txt']:
  p=C/'escape-patterns'/side/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(side+'\n')
with manual():
 compare(C/'escape-patterns/left',C/'escape-patterns/right');settled();p=r'{a\,b,c}.txt';st=pattern(p);event('escaped-alternative-ui',{'input':p,'state':st});snapshot('72-escaped-comma-alternative')
 specs=lambda s:{'type':'local','path':str(C/'escape-patterns'/s)}
 opened=invoke('open_session',left=specs('left'),right=specs('right'),options={'expandArchives':True,'ignored':'mark'},mergeBase=False)
 if opened['ok']:
  id=opened['value']['id'];opts={'includes':[p]};result=invoke('run_compare',id=id,options=opts,invalidate=False);event('escaped-alternative-backend',result);invoke('close_session',id=id)
 close_active()
