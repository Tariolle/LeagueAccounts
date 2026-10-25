use std::process::Command;

const DEFAULT_REPOSITORY: &str = "Tariolle/LeagueAccounts";
const REPOSITORY_ENV: &str = "LEAGUEACCOUNTS_UPDATE_REPOSITORY";

fn repository_name(value: &str) -> Option<String> {
    let remote = value.trim();
    let repository = remote.strip_suffix(".git").unwrap_or(remote);
    let mut parts = repository.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    (parts.next().is_none() && valid(owner) && valid(name)).then(|| format!("{owner}/{name}"))
}

fn origin_repository() -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    let remote = std::str::from_utf8(&output.stdout).ok()?.trim();
    let repository = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote.strip_prefix("git@github.com:"))?;
    repository_name(repository)
}

fn main() {
    println!("cargo:rerun-if-env-changed={REPOSITORY_ENV}");
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-path", "config"])
        .output()
    {
        if output.status.success() {
            if let Ok(path) = std::str::from_utf8(&output.stdout) {
                println!("cargo:rerun-if-changed={}", path.trim());
            }
        }
    }

    let repository = match std::env::var(REPOSITORY_ENV) {
        Ok(value) => repository_name(&value)
            .unwrap_or_else(|| panic!("{REPOSITORY_ENV} must be a GitHub owner/repository")),
        Err(_) => origin_repository().unwrap_or_else(|| DEFAULT_REPOSITORY.to_owned()),
    };
    println!("cargo:rustc-env=LEAGUEACCOUNTS_UPDATE_REPOSITORY={repository}");
    println!("Building League Accounts updates from {repository}");
}
