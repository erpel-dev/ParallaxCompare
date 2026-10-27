from manual import *
from PIL import ImageGrab
import zipfile,hashlib
for case in ['copy-match-right-only','copy-empty-subtrees']:
 for side in ['left','right']:
  root=C/case/side/'wrapper';root.mkdir(parents=True,exist_ok=True);(root/'normal.txt').write_text(side+' normal\n')
  if case=='copy-match-right-only':
   with zipfile.ZipFile(root/'bundle.zip','w')as z:
    z.writestr('hidden.bin',side+' hidden\n')
    if side=='right':z.writestr('keep.txt','only on right\n')
 if case=='copy-empty-subtrees':
  p=C/case/'left/wrapper/hidden/deep/secret.bin';p.parent.mkdir(parents=True,exist_ok=True);p.write_text('hidden\n')
with manual():
 for case in ['copy-match-right-only','copy-empty-subtrees']:
  compare(C/case/'left',C/case/'right');settled();pattern('*.txt');snapshot('20-'+case+'-before')
  js('const r=[...document.querySelectorAll(".page:not([hidden]) .viewport .row")].find(e=>e.querySelector(".name")?.innerText.includes("wrapper"));r.click()')
  click('Copy →');time.sleep(.25);ImageGrab.grab(xdisplay=':99').save(ROOT/'screenshots'/('21-'+case+'-confirmation.png'));desktop_key('Return');time.sleep(.4);settled()
  files=[str(p.relative_to(C/case/'right'))for p in(C/case/'right').rglob('*')]
  detail={'case':case,'targetTree':files,'text':visible()}
  if case=='copy-match-right-only':
   with zipfile.ZipFile(C/case/'right/wrapper/bundle.zip')as z:detail['targetArchive']={n:z.read(n).decode()for n in z.namelist()}
   detail['copiedNormal']=(C/case/'right/wrapper/normal.txt').read_text()
  event('filtered-copy-result',detail);snapshot('22-'+case+'-after');close_active()
