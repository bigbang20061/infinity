//! Decode blue.marshal files and print them as Python-like reprs.
//! Usage: marshal-dump [--check] <file>...   (--check: decode and re-encode, print only status)

fn main() {
    let mut check = false;
    let mut failed = 0;
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
            continue;
        }
        let bytes = match std::fs::read(&arg) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{arg}: {e}");
                failed += 1;
                continue;
            }
        };
        match marshal::loads(&bytes) {
            Ok(v) if check => {
                let again = marshal::loads(&marshal::dumps(&v));
                let ok = again.as_ref() == Ok(&v);
                if !ok {
                    failed += 1;
                }
                println!("{} {arg} ({} bytes)", if ok { "OK  " } else { "DIFF" }, bytes.len());
            }
            Ok(v) => println!("{arg}:\n{v:?}\n"),
            Err(e) => {
                failed += 1;
                println!("FAIL {arg}: {e}");
            }
        }
    }
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
