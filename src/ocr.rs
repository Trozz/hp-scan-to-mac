//! Runs `ocrmypdf` to add a text layer and fix page rotation and skew.

use anyhow::{Context, Result, bail};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
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

/// Kills ocrmypdf's whole process group (tesseract, ghostscript) and removes the
/// private work folder, whether the run finished, failed, timed out or was cancelled.
struct RunGuard {
    dir: PathBuf,
    group: Option<i32>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn run_in(
    parent: &Path,
    program: &str,
    pdf: &[u8],
    language: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let dir = parent.join(format!("hp-scan-ocr-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .context("creating OCR work folder")?;
    let mut guard = RunGuard { dir, group: None };
    let input = guard.dir.join("in.pdf");
    let output = guard.dir.join("out.pdf");
    tokio::fs::write(&input, pdf)
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
        .arg(&input)
        .arg(&output)
        .env("TMPDIR", &guard.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    guard.group = child.id().map(|pid| pid as i32);
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
    tokio::fs::read(&output).await.context("reading OCR output")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

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

    /// Waits up to 2s for a process to disappear.
    fn gone(pid_file: &Path) -> bool {
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..40 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// A fake ocrmypdf that starts a long-running child (like tesseract) and waits for it.
    fn slow_fake(dir: &Path) -> (String, PathBuf) {
        let pid = dir.join("child.pid");
        let prog = fake(
            dir,
            &format!("sleep 30 &\necho $! > '{}'\nwait", pid.display()),
        );
        (prog, pid)
    }

    #[tokio::test]
    async fn run_returns_ocr_output() {
        let dir = test_dir("ok");
        let args = dir.join("args.txt");
        let tmp = dir.join("tmp.txt");
        let prog = fake(
            &dir,
            &format!(
                "echo \"$@\" > '{}'\nls -ld \"$TMPDIR\" | cut -c1-10 > '{}'\necho \"$TMPDIR\" >> '{}'\nfor a; do in=$out; out=$a; done\ncp \"$in\" \"$out\"\nprintf OCR >> \"$out\"",
                args.display(),
                tmp.display(),
                tmp.display()
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
        let tmp = std::fs::read_to_string(tmp).unwrap();
        let (mode, tmpdir) = tmp.trim().split_once('\n').unwrap();
        assert_eq!(mode, "drwx------");
        assert!(Path::new(tmpdir).starts_with(&work), "{tmpdir}");
        assert!(args[6].starts_with(tmpdir) && args[7].starts_with(tmpdir));
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
        let (prog, pid) = slow_fake(&dir);
        let work = dir.join("work");
        let started = Instant::now();
        let err = run_in(&work, &prog, b"%PDF", "eng", Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(gone(&pid), "child of ocrmypdf still running");
        assert!(is_empty(&work));
    }

    #[tokio::test]
    async fn dropped_run_kills_and_cleans_up() {
        let dir = test_dir("drop");
        let (prog, pid) = slow_fake(&dir);
        let work = dir.join("work");
        let run = run_in(&work, &prog, b"%PDF", "eng", Duration::from_secs(60));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), run)
                .await
                .is_err()
        );
        assert!(gone(&pid), "child of ocrmypdf still running");
        assert!(is_empty(&work));
    }

    /// Runs the real ocrmypdf on a PDF made by `pdf::build` from the JPEG in $HP_SCAN_TEST_JPEG
    /// and writes the result next to it as `<jpeg>.ocr.pdf`.
    /// Run with: HP_SCAN_TEST_JPEG=page.jpg cargo test -- --ignored real_ocrmypdf
    #[tokio::test]
    #[ignore]
    async fn real_ocrmypdf_accepts_built_pdf() {
        let path = std::env::var("HP_SCAN_TEST_JPEG").unwrap();
        let mut jpeg = std::fs::read(&path).unwrap();
        let info = crate::jpeg::inspect_and_fix(&mut jpeg).unwrap();
        let pdf = crate::pdf::build(&[crate::pdf::Page {
            jpeg,
            info,
            dpi: 300,
        }]);
        let out = run(PROGRAM, &pdf, "eng", TIMEOUT).await.unwrap();
        assert!(out.starts_with(b"%PDF"));
        std::fs::write(format!("{path}.ocr.pdf"), out).unwrap();
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
