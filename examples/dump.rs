//! Prints decoded samples, for cross-checking against other readers:
//! `cargo run --example dump -- FILE SIGNAL_INDEX T0 T1`
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let file = oink_eeg::edf::EdfFile::open(&a[0]).expect("open");
    let s: usize = a[1].parse().unwrap();
    let (t0, t1): (f64, f64) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let w = &file.read_window(&[s], t0, t1).unwrap()[0];
    let vals: Vec<String> = w.data.iter().map(|v| format!("{v:.4}")).collect();
    println!("{} {}", w.start, vals.join(" "));
}
