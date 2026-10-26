use parallax_core::{source::{SourceSpec,SourceOptions},vfs::{Data,Change,EntryKind},progress::Progress};
use std::{env,fs};
fn main(){let dir=env::args().nth(1).unwrap();let open=|path:String|SourceSpec::Local{path}.open(SourceOptions::default()).unwrap();let archive=open(format!("{dir}/target.zip"));let folder=open(dir.clone());println!("Cached b={:?}",String::from_utf8_lossy(&archive.vfs.read(&archive.path("b.txt")).unwrap()));
folder.vfs.apply(vec![Change::Put{path:folder.path("target.zip"),kind:EntryKind::File,data:Data::from_vec(fs::read(format!("{dir}/replacement.zip")).unwrap()),mtime:Some(1700000000)}],&Progress::default()).unwrap();
println!("Fresh before inner save={:?}",String::from_utf8_lossy(&open(format!("{dir}/target.zip")).vfs.read(&archive.path("b.txt")).unwrap()));
archive.vfs.apply(vec![Change::Put{path:archive.path("a.txt"),kind:EntryKind::File,data:Data::from_vec(b"MY EDIT".to_vec()),mtime:None}],&Progress::default()).unwrap();
let fresh=open(format!("{dir}/target.zip"));for p in ["a.txt","b.txt"]{println!("After save {p}={:?}",String::from_utf8_lossy(&fresh.vfs.read(&fresh.path(p)).unwrap()));}}
