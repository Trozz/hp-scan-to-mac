//! Runs `ocrmypdf` to add a text layer and fix page rotation and skew.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

pub const PROGRAM: &str = "ocrmypdf";
pub const TIMEOUT: Duration = Duration::from_secs(300);

/// Fails with install instructions unless `program --version` runs.
pub fn check(program: &str) -> Result<()> {
    let ok = std::process::Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        return Ok(());
    }
    bail!(
        "OCR is on but {program} was not found. Install it (macOS: brew install ocrmypdf, \
         Debian/Ubuntu: apt install ocrmypdf) or use the -ocr Docker image"
    )
}

/// Returns the PDF with a text layer, rotated and deskewed.
pub async fn run(program: &str, pdf: &[u8], language: &str, timeout: Duration) -> Result<Vec<u8>> {
    run_in(&std::env::temp_dir(), program, pdf, language, timeout).await
}

async fn run_in(
    dir: &Path,
    program: &str,
    pdf: &[u8],
    language: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let id = uuid::Uuid::new_v4();
    let input = dir.join(format!("hp-scan-{id}.pdf"));
    let output = dir.join(format!("hp-scan-{id}-ocr.pdf"));
    let result = ocr_file(&input, &output, program, pdf, language, timeout).await;
    let _ = tokio::fs::remove_file(&input).await;
    let _ = tokio::fs::remove_file(&output).await;
    result
}

async fn ocr_file(
    input: &Path,
    output: &Path,
    program: &str,
    pdf: &[u8],
    language: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    tokio::fs::write(input, pdf)
        .await
        .context("writing OCR input")?;
    let child = tokio::process::Command::new(program)
        .args([
            "--rotate-pages",
            "--deskew",
            "--output-type",
            "pdf",
            "-l",
            language,
        ])
        .arg(input)
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    let Ok(done) = tokio::time::timeout(timeout, child.wait_with_output()).await else {
        bail!("{program} timed out after {}s", timeout.as_secs_f32());
    };
    let done = done.with_context(|| format!("waiting for {program}"))?;
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr);
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!("{program} exited with {}: {last}", done.status);
    }
    tokio::fs::read(output).await.context("reading OCR output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hpscan-ocr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        dir
    }

    fn fake(dir: &Path, body: &str) -> String {
        let path = dir.join("ocrmypdf");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    fn is_empty(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    #[tokio::test]
    async fn run_returns_ocr_output() {
        let dir = test_dir("ok");
        let args = dir.join("args.txt");
        let prog = fake(
            &dir,
            &format!(
                "echo \"$@\" > '{}'\nfor a; do in=$out; out=$a; done\ncp \"$in\" \"$out\"\nprintf OCR >> \"$out\"",
                args.display()
            ),
        );
        let work = dir.join("work");
        let out = run_in(
            &work,
            &prog,
            b"%PDF-1.4 x",
            "eng+deu",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(out, b"%PDF-1.4 xOCR");
        let args = std::fs::read_to_string(args).unwrap();
        let args: Vec<_> = args.split_whitespace().collect();
        assert_eq!(
            args[..6],
            [
                "--rotate-pages",
                "--deskew",
                "--output-type",
                "pdf",
                "-l",
                "eng+deu"
            ]
        );
        assert_eq!(args.len(), 8);
        assert!(is_empty(&work));
    }

    #[tokio::test]
    async fn run_reports_stderr_on_failure() {
        let dir = test_dir("fail");
        let prog = fake(&dir, "echo 'tesseract: language deu missing' >&2\nexit 3");
        let work = dir.join("work");
        let err = run_in(&work, &prog, b"%PDF", "deu", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("language deu missing"));
        assert!(is_empty(&work));
    }

    #[tokio::test]
    async fn run_times_out_and_kills() {
        let dir = test_dir("slow");
        let prog = fake(&dir, "sleep 30");
        let work = dir.join("work");
        let started = Instant::now();
        let err = run_in(&work, &prog, b"%PDF", "eng", Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(is_empty(&work));
    }

    #[test]
    fn check_names_install_options() {
        let err = check("/nonexistent/ocrmypdf").unwrap_err().to_string();
        for hint in ["brew install ocrmypdf", "apt install ocrmypdf", "-ocr"] {
            assert!(err.contains(hint), "{err}");
        }
        let dir = test_dir("check");
        assert!(check(&fake(&dir, "exit 0")).is_ok());
    }
}
