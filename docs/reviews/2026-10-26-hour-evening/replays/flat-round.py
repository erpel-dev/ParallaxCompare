from manual import *
for n in [100,150]:
 for side,prefix in [('left','old'),('right','new')]:
  for i in range(n):
   p=C/f'flat-{n}'/side/f'{prefix}{i:04}'/'index.txt';p.parent.mkdir(parents=True,exist_ok=True);p.write_text('identical file\n')
with manual():
 for n in [100,150]:
  compare(C/f'flat-{n}/left',C/f'flat-{n}/right');settled();t=time.monotonic();click('Flat');settled();time.sleep(.8)
  event('flat-ambiguous-group',{'filesPerSide':n,'seconds':time.monotonic()-t,'state':state(),'text':visible()[-1500:]});snapshot('60-flat-'+str(n));close_active()
