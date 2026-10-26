#!/usr/bin/env python3
"""Create disposable cases for the folder wildcard filter review."""
import argparse,pathlib,zipfile,subprocess,json
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--directory',type=pathlib.Path,required=True);p.add_argument('--large',action='store_true',help='Also create two 3,500-file trees');a=p.parse_args();R=a.directory.expanduser().resolve()
if R.exists()and(not R.is_dir()or any(R.iterdir())):p.error('--directory must be new or empty')
R.mkdir(parents=True,exist_ok=True)
def put(name,value):
 q=R/name;q.parent.mkdir(parents=True,exist_ok=True);q.write_text(value);return q
for side in ['left','right']:
 for name in ['a.txt','b.txt','é.txt','中.txt','😀.txt','Ä.TXT','main.java','Main.kt','src/nested/test.java','src/main.rs','src/nested/deep.rs','report final.txt','report,final.txt','report;final.txt']:
  put(f'patterns/{side}/{name}',side+' '+name+'\n')
 for case in ['archive-warning','nested-copy']:
  prefix=f'{case}/{side}/'+('wrapper/'if case=='nested-copy'else'');q=R/(prefix+'bundle.zip');q.parent.mkdir(parents=True,exist_ok=True)
  with zipfile.ZipFile(q,'w')as z:z.writestr('keep.txt',side+' VISIBLE\n');z.writestr('hidden.bin',side+' DO NOT REPLACE\n')
  if case=='nested-copy':put(prefix+'normal.txt',side+' normal\n')
 put(f'copy-control/{side}/dir/keep.txt',side+' keep\n');put(f'copy-control/{side}/dir/hidden.bin',side+' PROTECTED\n')
 if a.large:
  for i in range(3500):put(f'large/{side}/sub{i//100:02}/file{i:04}.txt',(side if i%4==0 else'same')+f' {i}\n')
for side in ['left','right']:
 put(f'live/{side}/keep.txt','same\n');put(f'live/{side}/existing.bin',side+' existing\n')
repo=R/'git';repo.mkdir()
def git(*args,data=None):return subprocess.run(['git','-C',str(repo),*args],input=data,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout.decode().strip()
git('init','-q','-b','main');blob=git('hash-object','-w','--stdin',data=b'nested stable\n');sub=git('mktree',data=f'100644 blob {blob}\tinner.txt\n'.encode());tree=git('mktree',data=f'040000 tree {sub}\tsub\n100644 blob {blob}\troot.txt\n'.encode())
for name in ['base','same']:git('update-ref','refs/tags/'+name,tree)
(R/'README.txt').write_text('Compare patterns/left and patterns/right for wildcard cases.\nCompare archive-warning/left vs right with *.txt then copy bundle.zip.\nCompare nested-copy/left vs right with *.txt then copy wrapper.\nCompare git@base with git@same and enter missing*.txt.\nAll files are disposable; no commits or identity configuration were made.\n');print(R)
