//! blue-patch: bypass the manifest entry hash check in the Infinity client's
//! 64-bit blue.dll so start.ini can be switched to `cryptoPack = Placebo`.
//!
//! Same idea as the legacy C# blue_patcher (neutralise the verification branch,
//! keep everything else), applied to the check this client actually has:
//! `VerifyManifestEntry` compares each manifested file's size and hash against
//! root:/manifest.dat and jumps to "Invalid manifest entry hash" on mismatch.
//! Both `jne`s are replaced with NOPs so the success path always runs.
//!
//! Usage: blue-patch [--check] <path to bin64/blue.dll>

use std::process::ExitCode;

struct Site {
    name: &'static str,
    /// Bytes before the jump, the original jump, bytes after; matched as one pattern.
    before: &'static [u8],
    jump: &'static [u8],
    after: &'static [u8],
}

/// x86 NOP opcode repeated to replace the six-byte conditional jump in blue.dll.
const NOP6: [u8; 6] = [0x90; 6];

/// Infinity blue.dll `VerifyManifestEntry` instruction signatures surrounding each jump.
const SITES: [Site; 2] = [
    Site {
        // cmp r8, [rsp+78h] ; jne fail ; call memcmp
        name: "manifest entry size compare",
        before: &[0x4C, 0x3B, 0x44, 0x24, 0x78],
        jump: &[0x0F, 0x85, 0xE7, 0x00, 0x00, 0x00],
        after: &[0xE8],
    },
    Site {
        // test eax, eax ; jne fail ; cmp qword [rsi+18h], 10h
        name: "manifest entry hash compare",
        before: &[0x85, 0xC0],
        jump: &[0x0F, 0x85, 0xDA, 0x00, 0x00, 0x00],
        after: &[0x48, 0x83, 0x7E, 0x18, 0x10],
    },
];

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i).collect()
}

enum State {
    Original(usize),
    Patched,
}

fn locate(data: &[u8], s: &Site) -> Result<State, String> {
    let pat = |jump: &[u8]| [s.before, jump, s.after].concat();
    let orig = find_all(data, &pat(s.jump));
    let done = find_all(data, &pat(&NOP6));
    match (orig.as_slice(), done.as_slice()) {
        ([off], []) => Ok(State::Original(off + s.before.len())),
        ([], [_]) => Ok(State::Patched),
        (o, p) => Err(format!("{}: expected exactly one match, found {} original / {} patched", s.name, o.len(), p.len())),
    }
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let check = args.first().is_some_and(|a| a == "--check");
    if check {
        args.remove(0);
    }
    let [path] = args.as_slice() else { return Err("usage: blue-patch [--check] <blue.dll>".into()) };
    let mut data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if !data.starts_with(b"MZ") {
        return Err(format!("{path}: not a PE file"));
    }

    let mut todo = Vec::new();
    for s in &SITES {
        match locate(&data, s)? {
            State::Original(off) => {
                println!("{:<30} at file offset 0x{off:X}: needs patch", s.name);
                todo.push(off);
            }
            State::Patched => println!("{:<30} already patched", s.name),
        }
    }
    if todo.is_empty() {
        println!("nothing to do: blue.dll is already patched");
        return Ok(());
    }
    if check {
        return Ok(());
    }

    let backup = format!("{path}.orig");
    if !std::path::Path::new(&backup).exists() {
        std::fs::write(&backup, &data).map_err(|e| format!("{backup}: {e}"))?;
        println!("backup written to {backup}");
    }
    for off in todo {
        data[off..off + NOP6.len()].copy_from_slice(&NOP6);
    }
    std::fs::write(path, &data).map_err(|e| format!("{path}: {e}"))?;
    println!("patched {path}");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
