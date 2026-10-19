use std::{path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=VERSION");
    println!("cargo:rerun-if-changed=version.sh");
    println!("cargo:rerun-if-env-changed=GITWATCH_COMMIT_COUNT");
    track_git_path("HEAD");
    track_git_path("packed-refs");

    let symbolic_head = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output();
    if let Ok(output) = symbolic_head
        && output.status.success()
    {
        let reference = String::from_utf8_lossy(&output.stdout);
        track_git_path(reference.trim());
    }

    let output = Command::new("bash")
        .arg("version.sh")
        .output()
        .expect("failed to run version.sh");
    if !output.status.success() {
        panic!(
            "version.sh failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let version =
        String::from_utf8(output.stdout).expect("version.sh returned a non-UTF-8 version");
    println!("cargo:rustc-env=GITWATCH_VERSION={}", version.trim());
}

fn track_git_path(name: &str) {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout);
        let path = PathBuf::from(path.trim());
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
