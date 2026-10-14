use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

#[derive(Debug, PartialEq)]
enum Target<'a> {
    Information,
    File(&'a OsStr),
    Fd(i32),
}

// util-linux 2.39.3 getopt_long actions; aliases with the same action are unambiguous.
const OPTIONS: &[(&[u8], u8)] = &[
    (b"shared", b's'),
    (b"exclusive", b'x'),
    (b"unlock", b'u'),
    (b"nonblocking", b'n'),
    (b"nb", b'n'),
    (b"timeout", b'w'),
    (b"wait", b'w'),
    (b"conflict-exit-code", b'E'),
    (b"close", b'o'),
    (b"no-fork", b'F'),
    (b"verbose", b'v'),
    (b"help", b'h'),
    (b"version", b'V'),
];

fn descriptor(mut bytes: &[u8]) -> Result<i32> {
    while matches!(bytes.first(), Some(b' ' | b'\t' | b'\n' | b'\r' | 11 | 12)) {
        bytes = &bytes[1..];
    }
    let negative = bytes.first() == Some(&b'-');
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        bytes = &bytes[1..];
    }
    ensure!(
        !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit),
        "invalid native file descriptor"
    );
    let mut fd = 0i32;
    for byte in bytes {
        fd = fd
            .checked_mul(10)
            .and_then(|n| n.checked_add(i32::from(byte - b'0')))
            .context("native file descriptor overflow")?;
    }
    ensure!(!negative || fd == 0, "negative native file descriptor");
    Ok(fd)
}

fn target(args: &[OsString]) -> Result<Target<'_>> {
    let mut i = 0;
    while i < args.len() {
        let bytes = args[i].as_bytes();
        if bytes == b"--" {
            i += 1;
            break;
        }
        if bytes == b"-" || !bytes.starts_with(b"-") {
            break;
        }
        if let Some(long) = bytes.strip_prefix(b"--") {
            let at = long.iter().position(|b| *b == b'=');
            let name = &long[..at.unwrap_or(long.len())];
            let action = if let Some((_, action)) = OPTIONS.iter().find(|(n, _)| *n == name) {
                *action
            } else {
                let mut matches = OPTIONS.iter().filter(|(n, _)| n.starts_with(name));
                let (_, action) = matches.next().context("unknown native flock option")?;
                ensure!(
                    !name.is_empty() && matches.all(|(_, other)| other == action),
                    "ambiguous native flock option"
                );
                *action
            };
            if matches!(action, b'w' | b'E') {
                if at.is_none() {
                    i += 1;
                    ensure!(i < args.len(), "missing native flock option value");
                }
            } else {
                ensure!(at.is_none(), "native flock option takes no value");
                if matches!(action, b'h' | b'V') {
                    return Ok(Target::Information);
                }
            }
        } else {
            for (at, action) in bytes[1..].iter().enumerate() {
                match action {
                    b's' | b'e' | b'x' | b'n' | b'o' | b'F' | b'u' => {}
                    b'h' | b'V' => return Ok(Target::Information),
                    b'w' | b'E' => {
                        if at + 2 == bytes.len() {
                            i += 1;
                            ensure!(i < args.len(), "missing native flock option value");
                        }
                        break;
                    }
                    _ => bail!("unknown native flock option"),
                }
            }
        }
        i += 1;
    }
    let operand = args.get(i).context("missing native flock target")?;
    if args.len() == i + 1 {
        return Ok(Target::Fd(descriptor(operand.as_bytes())?));
    }
    if matches!(args[i + 1].as_bytes(), b"-c" | b"--command") {
        ensure!(
            args.len() == i + 3,
            "native shell command needs exactly one argument"
        );
    }
    Ok(Target::File(operand))
}

pub fn check(file: &Path, args: &[OsString]) -> Result<i32> {
    guard(file, args).context(
        "flock guard refused; use slotr run --pool POOL --campaign CAMPAIGN --purpose PURPOSE -- COMMAND",
    )?;
    Ok(0)
}

fn guard(file: &Path, args: &[OsString]) -> Result<()> {
    let cfg = crate::config::load_fixed(file)?;
    let mut protected = Vec::new();
    for (pool, settings) in &cfg.pools {
        if let Some(lock) = &settings.legacy_lock
            && lock.queue_only
        {
            let path = Path::new(&lock.path);
            ensure!(
                path.is_absolute(),
                "queue-only pool {pool}: lock path must be absolute"
            );
            let canonical = fs::canonicalize(path)
                .with_context(|| format!("queue-only pool {pool}: resolve {}", path.display()))?;
            let meta = fs::metadata(&canonical)
                .with_context(|| format!("queue-only pool {pool}: inspect {}", path.display()))?;
            ensure!(
                meta.is_file(),
                "queue-only pool {pool}: {} must be an existing regular file",
                path.display()
            );
            protected.push((pool, path, meta.dev(), meta.ino()));
        }
    }
    ensure!(
        !protected.is_empty(),
        "flock guard policy needs at least one queue-only legacy lock"
    );
    let meta = match target(args)? {
        Target::Information => return Ok(()),
        Target::File(path) => match fs::metadata(path) {
            Ok(meta) => meta,
            // Stable protected inodes already exist; native decides how to open a missing target.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).context("cannot establish native flock file identity"),
        },
        Target::Fd(fd) => fs::metadata(format!("/proc/self/fd/{fd}"))
            .context("cannot establish inherited native flock descriptor identity")?,
    };
    for (pool, path, dev, ino) in protected {
        ensure!(
            (meta.dev(), meta.ino()) != (dev, ino),
            "queue-only pool {pool}: protected {} refuses native flock; use slotr run --pool {pool} --campaign CAMPAIGN --purpose PURPOSE -- COMMAND",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn native_target_table() {
        for options in [
            "",
            "-s",
            "-e",
            "-x",
            "-n",
            "-o",
            "-F",
            "-u",
            "-sxn",
            "-w 1",
            "-w1",
            "-E 33",
            "-E33",
            "-snw0",
            "-E -1",
            "-w --help",
            "--shared",
            "--exclusive",
            "--unlock",
            "--nonblocking",
            "--nb",
            "--nonblock",
            "--non",
            "--timeout 0",
            "--wait=0",
            "--conflict-exit-code=33",
            "--close",
            "--no-fork",
            "--verbose",
            "--sh",
            "--ex",
            "--un",
            "--ti=0",
            "--wa 0",
            "--con=33",
            "--cl",
            "--no-f",
            "--verb",
            "--",
        ] {
            let mut args: Vec<OsString> = options.split_whitespace().map(Into::into).collect();
            args.extend(["9".into(), "cmd".into(), "--help".into()]);
            assert_eq!(
                target(&args).unwrap(),
                Target::File(OsStr::new("9")),
                "{options}"
            );
        }
        for args in [
            vec!["--help", "--unknown"],
            vec!["--version", "--unknown"],
            vec!["-h?"],
            vec!["-V?"],
            vec!["--he"],
            vec!["--vers"],
        ] {
            let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
            assert_eq!(target(&args).unwrap(), Target::Information);
        }
        for args in [
            vec![],
            vec!["--"],
            vec!["-w"],
            vec!["-E"],
            vec!["--timeout"],
            vec!["--n", "9"],
            vec!["--no", "9"],
            vec!["--v", "9"],
            vec!["--c", "9"],
            vec!["--unknown", "--help"],
            vec!["-?h"],
            vec!["--help=x"],
            vec!["--shared="],
            vec!["--command", "9"],
            vec!["9", "-c"],
            vec!["9", "--command", "true", "extra"],
            vec!["--", "-1"],
            vec!["-0"],
            vec!["bad"],
            vec!["9 "],
            vec!["0x9"],
            vec!["2147483648"],
            vec!["+"],
        ] {
            let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
            assert!(target(&args).is_err(), "{args:?}");
        }
        for fd in ["9", "+9", "09", " 9", "\t9", "\x0b9", "\x0c9", "\r\n9"] {
            assert_eq!(target(&[fd.into()]).unwrap(), Target::Fd(9));
        }
        assert_eq!(target(&["--".into(), "-0".into()]).unwrap(), Target::Fd(0));
        for marker in ["-c", "--command"] {
            assert_eq!(
                target(&["file".into(), marker.into(), "true".into()]).unwrap(),
                Target::File(OsStr::new("file"))
            );
        }
        let path = OsString::from_vec(b"file-\xff".to_vec());
        assert_eq!(
            target(&[path.clone(), "cmd".into(), OsString::from_vec(vec![255])]).unwrap(),
            Target::File(&path)
        );
    }
}
