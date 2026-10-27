fn main() {let a:Vec<String>=std::env::args().collect();let mut w=sevenz_rust2::ArchiveWriter::create(&a[2]).unwrap();w.push_source_path(&a[1],|_|true).unwrap();w.finish().unwrap();}
