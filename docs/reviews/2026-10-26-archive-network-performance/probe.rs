use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},time::{Duration,Instant},path::PathBuf};
use parallax_core::{Result,source::{Source,SourceSpec,SourceInfo,SourceKind},vfs::{Vfs,Data,Entry,Change},vfs::local::LocalFs,vfs::mount::ArchiveMount,compare::{self,CompareOptions,Control},progress::Progress};
#[derive(Default)]struct Calls{stat:AtomicUsize,list:AtomicUsize,read:AtomicUsize,local:AtomicUsize}
impl Calls{fn reset(&self){for a in [&self.stat,&self.list,&self.read,&self.local]{a.store(0,Ordering::Relaxed);}}fn get(&self)->[usize;4]{[self.stat.load(Ordering::Relaxed),self.list.load(Ordering::Relaxed),self.read.load(Ordering::Relaxed),self.local.load(Ordering::Relaxed)]}}
struct Latent{inner:LocalFs,calls:Arc<Calls>,delay:Duration}
impl Vfs for Latent{
 fn label(&self)->String{self.inner.label()}
 fn stat(&self,p:&str)->Result<Entry>{self.calls.stat.fetch_add(1,Ordering::Relaxed);if !self.delay.is_zero(){std::thread::sleep(self.delay)}self.inner.stat(p)}
 fn list(&self,p:&str)->Result<Vec<Entry>>{self.calls.list.fetch_add(1,Ordering::Relaxed);self.inner.list(p)}
 fn read(&self,p:&str)->Result<Data>{self.calls.read.fetch_add(1,Ordering::Relaxed);self.inner.read(p)}
 fn local_path(&self,p:&str)->Option<PathBuf>{self.calls.local.fetch_add(1,Ordering::Relaxed);self.inner.local_path(p)}
 fn writable(&self)->bool{self.inner.writable()}
 fn apply(&self,c:Vec<Change>,p:&Progress)->Result<()>{self.inner.apply(c,p)}
 fn changed(&self,p:&str){self.inner.changed(p)}
}
fn source(root:&str,file:&str,delay:u64)->(Source,Arc<Calls>){let calls=Arc::new(Calls::default());let inner=LocalFs::new(root);let vfs=Arc::new(ArchiveMount::new(Arc::new(Latent{inner,calls:calls.clone(),delay:Duration::from_millis(delay)}),true));let s=Source{spec:SourceSpec::Local{path:format!("{root}/{file}")},vfs,root:file.into(),watch:None,info:SourceInfo{kind:SourceKind::Archive,display:file.into(),rev:None,commit:None,commit_summary:None,writable:true}};(s,calls)}
fn main(){let a:Vec<String>=std::env::args().collect();let root=&a[1];let count=&a[2];let delay:u64=a[3].parse().unwrap();let mode=&a[4];let left=format!("left-{count}.zip");let right=format!("{}-{count}.zip",if mode=="equal"||mode=="verify"{"equal"}else{"right"});let(l,lc)=source(root,&left,delay);let(r,rc)=source(root,&right,delay);let mut opts=CompareOptions::default();opts.verify_checksums=mode=="verify";if mode=="bytes"{opts.ignore_line_endings=false;opts.compare_contents=false;}
 for round in 0..2{lc.reset();rc.reset();let t=Instant::now();let result=compare::compare(&l,&r,&opts,&Control::default()).unwrap();println!("count={count} delay_ms={delay} mode={mode} round={round} seconds={:.6} left_calls={:?} right_calls={:?} files={} same={} different={}",t.elapsed().as_secs_f64(),lc.get(),rc.get(),result.summary.files,result.summary.same,result.summary.different);}
}
