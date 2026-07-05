use std::{
    fmt,
    io::Write as _,
    process::{Command, Stdio},
};

use color_eyre::eyre::{Result, eyre};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardMethod {
    Pbcopy,
    WlCopy,
    Xclip,
    Osc52,
}

impl fmt::Display for ClipboardMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pbcopy => "pbcopy",
            Self::WlCopy => "wl-copy",
            Self::Xclip => "xclip",
            Self::Osc52 => "OSC52 (/dev/tty)",
        })
    }
}

pub fn select_clipboard_method(mut available: impl FnMut(&str) -> bool) -> ClipboardMethod {
    if available("pbcopy") {
        ClipboardMethod::Pbcopy
    } else if available("wl-copy") {
        ClipboardMethod::WlCopy
    } else if available("xclip") {
        ClipboardMethod::Xclip
    } else {
        ClipboardMethod::Osc52
    }
}

pub fn copy_to_clipboard(text: &str) -> Result<ClipboardMethod> {
    let method = select_clipboard_method(command_exists);
    match method {
        ClipboardMethod::Pbcopy => pipe_to("pbcopy", &[], text)?,
        ClipboardMethod::WlCopy => pipe_to("wl-copy", &[], text)?,
        ClipboardMethod::Xclip => pipe_to("xclip", &["-selection", "clipboard"], text)?,
        ClipboardMethod::Osc52 => write_osc52_to_tty(text)?,
    }
    Ok(method)
}

/// Check for `name` on `PATH` without spawning it: actually running a
/// candidate (even with `--help`) can have side effects — e.g. `pbcopy`
/// ignores unknown flags and clobbers the clipboard with its (empty) stdin.
fn command_exists(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(name);
        candidate
            .metadata()
            .map(|meta| {
                use std::os::unix::fs::PermissionsExt as _;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            })
            .unwrap_or(false)
    })
}

fn pipe_to(command: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| eyre!("failed to open {command} stdin"))?
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(eyre!("{command} exited with {status}"))
    }
}

fn write_osc52_to_tty(text: &str) -> Result<()> {
    let encoded = base64_encode(text.as_bytes());
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    write!(tty, "\x1b]52;c;{}\x07", encoded)?;
    Ok(())
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_selection_prefers_pbcopy_then_wl_copy_then_xclip_then_osc52() {
        assert_eq!(
            select_clipboard_method(|name| name == "pbcopy"),
            ClipboardMethod::Pbcopy
        );
        assert_eq!(
            select_clipboard_method(|name| name == "wl-copy"),
            ClipboardMethod::WlCopy
        );
        assert_eq!(
            select_clipboard_method(|name| name == "xclip"),
            ClipboardMethod::Xclip
        );
        assert_eq!(select_clipboard_method(|_| false), ClipboardMethod::Osc52);
    }
}
