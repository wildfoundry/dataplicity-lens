use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_ACTIONS");
    println!("cargo:rerun-if-env-changed=LENS_GIT_SHA");
    println!("cargo:rerun-if-env-changed=LENS_BUILT_BY");
    rerun_for_git_metadata();

    let commit = env::var("LENS_GIT_SHA")
        .or_else(|_| env::var("GITHUB_SHA"))
        .ok()
        .or_else(git_sha)
        .unwrap_or_else(|| "unknown".to_owned());
    let built_by = env::var("LENS_BUILT_BY").unwrap_or_else(|_| {
        if env::var_os("GITHUB_ACTIONS").is_some() {
            "GitHub Actions".to_owned()
        } else {
            "local build".to_owned()
        }
    });
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_owned());

    println!("cargo:rustc-env=LENS_GIT_SHA={commit}");
    println!("cargo:rustc-env=LENS_BUILT_BY={built_by}");
    println!("cargo:rustc-env=LENS_TARGET={target}");
}

fn rerun_for_git_metadata() {
    let head = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output();
    if let Ok(head) = head
        && head.status.success()
    {
        let path = String::from_utf8_lossy(&head.stdout);
        let path = path.trim();
        if !path.is_empty() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let symbolic = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output();
    if let Ok(symbolic) = symbolic
        && symbolic.status.success()
    {
        let reference = String::from_utf8_lossy(&symbolic.stdout);
        let reference = reference.trim();
        if !reference.is_empty()
            && let Ok(path) = Command::new("git")
                .args(["rev-parse", "--git-path", reference])
                .output()
            && path.status.success()
        {
            let path = String::from_utf8_lossy(&path.stdout);
            let path = path.trim();
            if !path.is_empty() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
}

fn git_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
