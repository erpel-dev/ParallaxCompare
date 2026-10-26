#!/usr/bin/env python3
"""Create small ZIPs for the archive metadata-latency benchmark."""
import argparse,pathlib,zipfile
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--directory',type=pathlib.Path,required=True);a=p.parse_args();r=a.directory.resolve()
if r.exists()and(not r.is_dir()or any(r.iterdir())):p.error('--directory must be new or empty')
r.mkdir(parents=True,exist_ok=True)
for n in [100,1000]:
 for side in ['left','right','equal']:
  with zipfile.ZipFile(r/f'{side}-{n}.zip','w',compression=zipfile.ZIP_DEFLATED)as z:
   for i in range(n):z.writestr(f'dir/f{i:05}.txt',('left'if side=='equal'else side)+f' contents {i}\n')
print(r)
