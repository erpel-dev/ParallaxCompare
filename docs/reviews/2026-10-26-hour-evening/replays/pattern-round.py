from manual import *
for side in ['left','right']:
 for name in ['a.txt',' leading.txt','trailing.txt ','report final.txt','report,final.txt','"quote.txt']:
  p=C/'literal-only'/side/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(side+' '+name+'\n')
with manual():
 compare(C/'literal-only/left',C/'literal-only/right');settled();snapshot('01-literal-baseline');pattern('*.txt')
 event('before-invalid-quote',state());pattern('"');event('invalid-quote',state());pattern('*.txt');event('corrected-same-pattern',state());snapshot('02-error-after-correcting-quote')
 click('⟳ Refresh');settled();event('refreshed-error',state())
 for p in ['" leading.txt"','"trailing.txt "','"report final.txt"','"report,final.txt"',"'\"quote.txt'",'*.txt*']:
  event('literal-exact-pattern',{'input':p,**pattern(p)})
  if p in ['" leading.txt"','"trailing.txt "']:snapshot('03-literal-whitespace-'+str(p.startswith('" leading')))
 close_active()
