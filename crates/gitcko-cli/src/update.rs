// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko update` (LOCAL_DEPLOYMENT §9, P5_PLAN §5.4).
//!
//! A script install is replaced only after the minisign signature and the SHA-256
//! both match. A bad checksum, a missing signature, or another key leaves the
//! binary that is already there. Other install methods are not downloaded: the
//! command prints what to run instead.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use crate::Io;
use crate::cli::UpdateArgs;
use crate::verify::{
    self, Receipt, VerifyFailure, is_release_name, parse_sums, sha256, verify_signature,
};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_URL: &str = "https://gitcko.dev/releases";

/// Where this binary was installed, as far as `gitcko update` can tell.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstallMethod {
    /// Receipt next to the binary, plus the directory that holds the signed sums.
    Script(PathBuf),
    Npm,
    Homebrew,
    Cargo,
    Plugin,
    /// No receipt and no known installer path.
    Unknown,
    /// A receipt file is present and is not safe to trust.
    RejectedReceipt,
}

/// Run `gitcko update`. `exe` is the binary to replace; the process path is used
/// when it is `None`.
pub fn run<O, E>(
    args: &UpdateArgs,
    exe: Option<&Path>,
    env: &BTreeMap<String, String>,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    let owned;
    let exe = match exe {
        Some(path) => path,
        None => {
            owned = match std::env::current_exe() {
                Ok(path) => path,
                Err(_) => {
                    let _ = writeln!(lock(&io.stderr), "could not find this binary");
                    return 1;
                }
            };
            owned.as_path()
        }
    };
    match detect_install_method(exe) {
        InstallMethod::RejectedReceipt => fail(io, "install receipt rejected"),
        InstallMethod::Unknown => fail(io, "this install cannot be updated here"),
        InstallMethod::Homebrew => ok(io, "brew upgrade gitcko"),
        InstallMethod::Cargo => ok(io, "cargo install gitcko --locked"),
        InstallMethod::Plugin => ok(io, "update the plugin"),
        InstallMethod::Npm => npm(args.check, env, io),
        InstallMethod::Script(_) => match script_from_env(exe, env, args.check, true) {
            Ok(message) => ok(io, &message),
            Err(err) => fail(io, err.detail()),
        },
    }
}

pub(crate) fn detect_install_method(exe: &Path) -> InstallMethod {
    match verify::read_receipt(exe) {
        Receipt::Rejected => return InstallMethod::RejectedReceipt,
        Receipt::Script(dir) => return InstallMethod::Script(dir),
        Receipt::Absent => {}
    }
    let parts: Vec<String> = exe
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    let has = |name: &str| parts.iter().any(|part| part == name);
    if has("node_modules") {
        InstallMethod::Npm
    } else if has(".cargo") && has("bin") {
        InstallMethod::Cargo
    } else if has("cellar") || has("homebrew") || has("linuxbrew") || has(".linuxbrew") {
        InstallMethod::Homebrew
    } else if has(".claude")
        || has(".codex")
        || has("claude-plugins")
        || (has(".cursor") && has("extensions"))
    {
        InstallMethod::Plugin
    } else {
        InstallMethod::Unknown
    }
}

/// The release target this process would download. Linux builds are the musl ones.
pub fn host_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn script_from_env(
    exe: &Path,
    env: &BTreeMap<String, String>,
    check_only: bool,
    commit: bool,
) -> Result<String, UpdateError> {
    let url = env
        .get("GITCKO_DOWNLOAD_URL")
        .map(String::as_str)
        .unwrap_or(DEFAULT_URL);
    let base = classify_base(url)?;
    let target = host_target().ok_or(UpdateError::Unsupported)?;
    script_update(
        exe,
        &base,
        target,
        env!("CARGO_PKG_VERSION"),
        check_only,
        commit,
    )
}

/// Download, verify, and maybe replace `exe`. `commit` false writes the new bytes
/// aside and then deletes them, which is how the swap-failure test leaves the old file.
pub(crate) fn script_update(
    exe: &Path,
    base: &Base,
    target: &str,
    current_version: &str,
    check_only: bool,
    commit: bool,
) -> Result<String, UpdateError> {
    let sums = fetch(base, "SHA256SUMS")?;
    let signature = fetch(base, "SHA256SUMS.minisig")?;
    verify_signature(&sums, &signature, verify::pinned_public_key())?;
    let files = parse_sums(&sums)?;
    let version_bytes = fetch(base, "VERSION")?;
    let version_hash = files.get("VERSION").ok_or(VerifyFailure::Checksum)?;
    if sha256(&version_bytes) != *version_hash {
        return Err(VerifyFailure::Checksum.into());
    }
    let version = parse_version(&version_bytes)?;
    match version_cmp(&version, current_version) {
        std::cmp::Ordering::Less => {
            return Ok(format!("up to date: {current_version}"));
        }
        std::cmp::Ordering::Equal if check_only => {
            return Ok(format!("up to date: {version}"));
        }
        std::cmp::Ordering::Equal => {}
        std::cmp::Ordering::Greater if check_only => {
            return Ok(format!("update available: {version}"));
        }
        std::cmp::Ordering::Greater => {}
    }
    let (archive_name, bin_name) = artifact_names(target)?;
    let archive = fetch(base, &archive_name)?;
    let archive_hash = files.get(&archive_name).ok_or(VerifyFailure::Checksum)?;
    if sha256(&archive) != *archive_hash {
        return Err(VerifyFailure::Checksum.into());
    }
    let binary = extract(&archive_name, &archive)?;
    let bin_hash = files.get(&bin_name).ok_or(VerifyFailure::Checksum)?;
    if sha256(&binary) != *bin_hash {
        return Err(VerifyFailure::Checksum.into());
    }
    if fs::read(exe).ok().as_deref() == Some(binary.as_slice()) {
        return Ok(format!("up to date: {current_version}"));
    }
    replace_executable(exe, &binary, commit)?;
    Ok(format!("updated: {version}"))
}

fn artifact_names(target: &str) -> Result<(String, String), UpdateError> {
    if !is_release_name(target) || target.contains('.') {
        return Err(UpdateError::Unsupported);
    }
    let archive = if target.contains("windows") {
        format!("gitcko-{target}.zip")
    } else {
        format!("gitcko-{target}.tar.gz")
    };
    Ok((archive, format!("gitcko-{target}.bin")))
}

/// A download base that has already been allowed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Base {
    /// A local directory (`file://` or an absolute path).
    Dir(PathBuf),
    /// `https://` anywhere, or `http://` only for loopback.
    Http(String),
}

/// Refuse cleartext to a public host, userinfo, and relative paths.
pub(crate) fn classify_base(raw: &str) -> Result<Base, UpdateError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_control) {
        return Err(UpdateError::Refused);
    }
    if let Some(rest) = raw.strip_prefix("file://") {
        return file_base(rest);
    }
    if is_absolute_path(raw) {
        return Ok(Base::Dir(PathBuf::from(decode_spaces(raw)?)));
    }
    if let Some(rest) = raw.strip_prefix("https://") {
        check_authority(rest, true)?;
        return Ok(Base::Http(trim_slash(raw)));
    }
    if let Some(rest) = raw.strip_prefix("http://") {
        check_authority(rest, false)?;
        return Ok(Base::Http(trim_slash(raw)));
    }
    Err(UpdateError::Refused)
}

fn file_base(rest: &str) -> Result<Base, UpdateError> {
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if let Some(path) = rest.strip_prefix('/') {
        format!("/{path}")
    } else {
        return Err(UpdateError::Refused);
    };
    Ok(Base::Dir(PathBuf::from(decode_spaces(&path)?)))
}

fn decode_spaces(text: &str) -> Result<String, UpdateError> {
    let decoded = text.replace("%20", " ");
    if decoded.contains('%') {
        return Err(UpdateError::Refused);
    }
    Ok(decoded)
}

fn is_absolute_path(raw: &str) -> bool {
    raw.starts_with('/') || is_windows_drive(raw)
}

fn is_windows_drive(raw: &str) -> bool {
    let mut chars = raw.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) => letter.is_ascii_alphabetic(),
        _ => false,
    }
}

fn check_authority(rest: &str, https: bool) -> Result<(), UpdateError> {
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains(' ') {
        return Err(UpdateError::Refused);
    }
    let (host, port) = split_host(authority)?;
    if port.is_some_and(|port| port.is_empty() || !port.chars().all(|c| c.is_ascii_digit())) {
        return Err(UpdateError::Refused);
    }
    if host.is_empty() || (!https && host != "127.0.0.1" && host != "localhost" && host != "[::1]")
    {
        return Err(UpdateError::Refused);
    }
    Ok(())
}

fn split_host(authority: &str) -> Result<(&str, Option<&str>), UpdateError> {
    if let Some(rest) = authority.strip_prefix("[::1]") {
        if rest.is_empty() {
            return Ok(("[::1]", None));
        }
        return rest
            .strip_prefix(':')
            .map(|port| ("[::1]", Some(port)))
            .ok_or(UpdateError::Refused);
    }
    if let Some((host, port)) = authority.split_once(':') {
        return Ok((host, Some(port)));
    }
    Ok((authority, None))
}

fn trim_slash(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

fn fetch(base: &Base, name: &str) -> Result<Vec<u8>, UpdateError> {
    if !is_release_name(name) || name == "." || name == ".." {
        return Err(UpdateError::Refused);
    }
    match base {
        Base::Dir(root) => read_local(root, name),
        Base::Http(url) => block_on(http_get(&format!("{url}/{name}"))),
    }
}

fn read_local(root: &Path, name: &str) -> Result<Vec<u8>, UpdateError> {
    let root = root.canonicalize().map_err(|_| UpdateError::Download)?;
    let path = root.join(name);
    let path = path.canonicalize().map_err(|_| UpdateError::Download)?;
    if !path.starts_with(&root) {
        return Err(UpdateError::Refused);
    }
    let meta = fs::symlink_metadata(&path).map_err(|_| UpdateError::Download)?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err(UpdateError::Download);
    }
    let bytes = fs::read(&path).map_err(|_| UpdateError::Download)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(UpdateError::Download);
    }
    Ok(bytes)
}

fn http_client() -> Result<reqwest::Client, UpdateError> {
    // Same reason as the model adapters: reqwest is built without a default provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| UpdateError::Download)
}

async fn http_get(url: &str) -> Result<Vec<u8>, UpdateError> {
    let client = http_client()?;
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(err) if err.is_redirect() => return Err(UpdateError::Refused),
        Err(_) => return Err(UpdateError::Download),
    };
    // `redirect::Policy::none` returns the 3xx response. Do not read it: the
    // body and the Location header are not trusted, and the next URL is not fetched.
    if response.status().is_redirection() {
        return Err(UpdateError::Refused);
    }
    if !response.status().is_success() {
        return Err(UpdateError::Download);
    }
    if response.content_length().is_some_and(|len| len > MAX_BYTES) {
        return Err(UpdateError::Download);
    }
    let bytes = response.bytes().await.map_err(|_| UpdateError::Download)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(UpdateError::Download);
    }
    Ok(bytes.to_vec())
}

fn block_on<T>(
    fut: impl std::future::Future<Output = Result<T, UpdateError>>,
) -> Result<T, UpdateError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| UpdateError::Download)?;
    runtime.block_on(fut)
}

fn extract(archive_name: &str, bytes: &[u8]) -> Result<Vec<u8>, UpdateError> {
    if archive_name.ends_with(".tar.gz") {
        extract_tar_gz(bytes)
    } else if archive_name.ends_with(".zip") {
        extract_zip(bytes)
    } else {
        Err(UpdateError::Archive)
    }
}

fn extract_tar_gz(bytes: &[u8]) -> Result<Vec<u8>, UpdateError> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut found = None;
    for entry in archive.entries().map_err(|_| UpdateError::Archive)? {
        let entry = entry.map_err(|_| UpdateError::Archive)?;
        if !entry.header().entry_type().is_file() {
            return Err(UpdateError::Archive);
        }
        let path = entry.path().map_err(|_| UpdateError::Archive)?;
        if !single_binary_name(&path) {
            return Err(UpdateError::Archive);
        }
        if found.is_some() {
            return Err(UpdateError::Archive);
        }
        let mut buf = Vec::new();
        entry
            .take(MAX_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|_| UpdateError::Archive)?;
        if buf.len() as u64 > MAX_BYTES {
            return Err(UpdateError::Archive);
        }
        found = Some(buf);
    }
    found.ok_or(UpdateError::Archive)
}

fn extract_zip(bytes: &[u8]) -> Result<Vec<u8>, UpdateError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| UpdateError::Archive)?;
    if archive.len() != 1 {
        return Err(UpdateError::Archive);
    }
    let file = archive.by_index(0).map_err(|_| UpdateError::Archive)?;
    let Some(path) = file.enclosed_name() else {
        return Err(UpdateError::Archive);
    };
    if !single_binary_name(&path) || file.size() > MAX_BYTES {
        return Err(UpdateError::Archive);
    }
    let mut buf = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| UpdateError::Archive)?;
    if buf.len() as u64 > MAX_BYTES {
        return Err(UpdateError::Archive);
    }
    Ok(buf)
}

fn single_binary_name(path: &Path) -> bool {
    let mut names = path.components().filter_map(|component| match component {
        Component::Normal(name) => Some(name),
        Component::CurDir => None,
        _ => Some(std::ffi::OsStr::new("..")),
    });
    match (names.next(), names.next()) {
        (Some(name), None) => name == "gitcko" || name == "gitcko.exe",
        _ => false,
    }
}

/// Write `bytes` over `target` by renaming in the same directory.
/// `commit` false deletes the staged file and returns an error, leaving `target` as it was.
pub(crate) fn replace_executable(
    target: &Path,
    bytes: &[u8],
    commit: bool,
) -> Result<(), UpdateError> {
    if fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(UpdateError::Swap);
    }
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(UpdateError::Swap)?;
    let mut tmp_name = target.file_name().ok_or(UpdateError::Swap)?.to_os_string();
    tmp_name.push(".new");
    let tmp = parent.join(tmp_name);
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|_| UpdateError::Swap)?;
        file.write_all(bytes).map_err(|_| UpdateError::Swap)?;
        file.sync_all().map_err(|_| UpdateError::Swap)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&tmp)
            .map_err(|_| UpdateError::Swap)?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms).map_err(|_| UpdateError::Swap)?;
    }
    if !commit {
        let _ = fs::remove_file(&tmp);
        return Err(UpdateError::Swap);
    }
    if let Err(err) = commit_replace(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn commit_replace(tmp: &Path, target: &Path) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        let parent = target.parent().ok_or(UpdateError::Swap)?;
        let mut old_name = target.file_name().ok_or(UpdateError::Swap)?.to_os_string();
        old_name.push(".old");
        let old = parent.join(old_name);
        let had = target.exists();
        if had {
            fs::rename(target, &old).map_err(|_| UpdateError::Swap)?;
        }
        if fs::rename(tmp, target).is_err() {
            if had {
                let _ = fs::rename(&old, target);
            }
            return Err(UpdateError::Swap);
        }
        if had {
            let _ = fs::remove_file(&old);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, target).map_err(|_| UpdateError::Swap)
    }
}

fn version_cmp(remote: &str, current: &str) -> std::cmp::Ordering {
    let remote_parts = remote
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect::<Vec<_>>();
    let current_parts = current
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect::<Vec<_>>();
    let slots = remote_parts.len().max(current_parts.len());
    for index in 0..slots {
        let remote = remote_parts.get(index).copied().unwrap_or(0);
        let current = current_parts.get(index).copied().unwrap_or(0);
        match remote.cmp(&current) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn parse_version(bytes: &[u8]) -> Result<String, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|_| UpdateError::Version)?;
    let mut lines = text.lines().filter(|line| !line.is_empty());
    let Some(line) = lines.next() else {
        return Err(UpdateError::Version);
    };
    if lines.next().is_some()
        || line.is_empty()
        || line.len() > 32
        || !line
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
    {
        return Err(UpdateError::Version);
    }
    Ok(line.to_string())
}

fn npm<O, E>(check: bool, env: &BTreeMap<String, String>, io: &Io<O, E>) -> u8
where
    O: Write,
    E: Write,
{
    if check {
        return ok(io, "npm i -g gitcko@latest");
    }
    let mut cmd = Command::new("npm");
    cmd.args(["i", "-g", "gitcko@latest"]);
    cmd.env_clear();
    for (key, value) in env {
        if key == "GITCKO_DOWNLOAD_URL" {
            continue;
        }
        cmd.env(key, value);
    }
    match cmd.status() {
        Ok(status) if status.success() => ok(io, "updated: npm"),
        _ => fail(io, "npm update failed"),
    }
}

fn ok<O: Write, E>(io: &Io<O, E>, message: &str) -> u8 {
    let _ = writeln!(lock(&io.stdout), "{message}");
    0
}

fn fail<O, E: Write>(io: &Io<O, E>, message: &str) -> u8 {
    let _ = writeln!(lock(&io.stderr), "{message}");
    1
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateError {
    Verify(VerifyFailure),
    Refused,
    Download,
    Archive,
    Swap,
    Version,
    Unsupported,
}

impl UpdateError {
    fn detail(self) -> &'static str {
        match self {
            Self::Verify(err) => err.detail(),
            Self::Refused => "download refused",
            Self::Download => "download failed",
            Self::Archive => "archive rejected",
            Self::Swap => "install failed",
            Self::Version => "version file rejected",
            Self::Unsupported => "this build does not ship a binary for this system",
        }
    }
}

impl From<VerifyFailure> for UpdateError {
    fn from(err: VerifyFailure) -> Self {
        Self::Verify(err)
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::doctor_signature_check;
    use std::sync::atomic::{AtomicU16, Ordering};

    #[test]
    fn install_method_follows_the_path_and_a_script_receipt_wins() {
        let root = tempfile::tempdir().unwrap();
        let brew = root.path().join("homebrew/Cellar/gitcko/0.0.0/bin/gitcko");
        fs::create_dir_all(brew.parent().unwrap()).unwrap();
        fs::write(&brew, b"brew").unwrap();
        assert_eq!(detect_install_method(&brew), InstallMethod::Homebrew);

        let cargo = root.path().join(".cargo/bin/gitcko");
        fs::create_dir_all(cargo.parent().unwrap()).unwrap();
        fs::write(&cargo, b"cargo").unwrap();
        assert_eq!(detect_install_method(&cargo), InstallMethod::Cargo);

        let npm = root.path().join("node_modules/gitcko/gitcko");
        fs::create_dir_all(npm.parent().unwrap()).unwrap();
        fs::write(&npm, b"npm").unwrap();
        assert_eq!(detect_install_method(&npm), InstallMethod::Npm);

        let plugin = root.path().join(".cursor/extensions/gitcko/gitcko");
        fs::create_dir_all(plugin.parent().unwrap()).unwrap();
        fs::write(&plugin, b"plugin").unwrap();
        assert_eq!(detect_install_method(&plugin), InstallMethod::Plugin);

        let copied = root.path().join("copied/gitcko");
        fs::create_dir_all(copied.parent().unwrap()).unwrap();
        fs::write(&copied, b"copied").unwrap();
        assert_eq!(detect_install_method(&copied), InstallMethod::Unknown);

        let release = root.path().join("release");
        fs::create_dir_all(&release).unwrap();
        fs::write(
            copied.parent().unwrap().join("gitcko-install.txt"),
            format!("method=script\nrelease_dir={}\n", release.display()),
        )
        .unwrap();
        assert!(matches!(
            detect_install_method(&copied),
            InstallMethod::Script(_)
        ));
    }

    #[test]
    fn download_urls_that_are_not_https_file_or_loopback_are_refused() {
        assert!(matches!(
            classify_base("https://gitcko.dev/releases"),
            Ok(Base::Http(_))
        ));
        assert!(matches!(
            classify_base("http://127.0.0.1:9/releases"),
            Ok(Base::Http(_))
        ));
        assert!(matches!(
            classify_base("http://localhost/releases"),
            Ok(Base::Http(_))
        ));
        assert!(matches!(
            classify_base("http://[::1]:8080/releases"),
            Ok(Base::Http(_))
        ));
        assert!(matches!(classify_base("/tmp/releases"), Ok(Base::Dir(_))));
        assert!(matches!(
            classify_base("file:///tmp/releases"),
            Ok(Base::Dir(path)) if path == Path::new("/tmp/releases")
        ));
        for refused in [
            "",
            "http://example.com/canary_update_secret",
            "http://127.0.0.1.example/releases",
            "https://user:canary_update_secret@gitcko.dev/releases",
            "file://evil/tmp",
            "ftp://gitcko.dev/releases",
            "releases",
            "file:///tmp/%2e%2e/secret",
        ] {
            let err = classify_base(refused).expect_err(refused);
            assert_eq!(err, UpdateError::Refused);
            assert!(!err.detail().contains("canary_"), "{refused}");
        }
    }

    #[test]
    fn a_name_or_symlink_that_leaves_the_release_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let err = fetch(&Base::Dir(root.path().to_path_buf()), "..").unwrap_err();
        assert_eq!(err, UpdateError::Refused);
        assert!(!err.detail().contains("canary_"));

        #[cfg(unix)]
        {
            let outside = root.path().join("secret");
            fs::write(&outside, b"canary_update_secret").unwrap();
            let rel = root.path().join("rel");
            fs::create_dir(&rel).unwrap();
            std::os::unix::fs::symlink(&outside, rel.join("SHA256SUMS")).unwrap();
            let exe = root.path().join("gitcko");
            fs::write(&exe, b"stay").unwrap();
            let err = script_update(
                &exe,
                &Base::Dir(rel),
                "aarch64-apple-darwin",
                "0.0.0",
                true,
                true,
            )
            .unwrap_err();
            assert_eq!(err, UpdateError::Refused);
            assert!(!err.detail().contains("canary_"));
            assert_eq!(fs::read(&exe).unwrap(), b"stay");
        }
    }

    #[test]
    fn a_redirect_is_refused_and_a_loopback_body_is_returned() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let body = b"loopback-ok";
        let url = serve(200, "", body);
        let got = block_on(http_get(&format!("{url}/SHA256SUMS"))).expect("200");
        assert_eq!(got, body);
        let redirect = serve(
            302,
            "Location: http://example.com/canary_update_secret\r\n",
            b"",
        );
        let err = block_on(http_get(&format!("{redirect}/SHA256SUMS"))).expect_err("redirect");
        assert_eq!(err, UpdateError::Refused);
        assert!(!err.detail().contains("canary_"));
    }

    #[test]
    fn a_good_release_replaces_the_binary_and_the_refusals_do_not() {
        let root = tempfile::tempdir().unwrap();
        let target = "aarch64-apple-darwin";
        let new_bytes = b"new-gitcko-bytes";
        let release = root.path().join("release");
        copy_release("integrations/install/testdata/release-good", &release);
        let install = root.path().join("dir with spaces");
        fs::create_dir_all(&install).unwrap();
        let exe = install.join("gitcko");
        fs::write(&exe, b"old-gitcko-bytes").unwrap();
        let base = Base::Dir(release.clone());

        let archive = release.join("gitcko-aarch64-apple-darwin.tar.gz");
        let saved = fs::read(&archive).unwrap();
        fs::remove_file(&archive).unwrap();
        let check = script_update(&exe, &base, target, "0.0.0", true, true).unwrap();
        assert_eq!(check, "update available: 9.9.9");
        assert_eq!(fs::read(&exe).unwrap(), b"old-gitcko-bytes");
        fs::write(&archive, saved).unwrap();

        let updated = script_update(&exe, &base, target, "0.0.0", false, true).unwrap();
        assert_eq!(updated, "updated: 9.9.9");
        assert_eq!(fs::read(&exe).unwrap(), new_bytes);

        let again = script_update(&exe, &base, target, "0.0.0", false, true).unwrap();
        assert!(again.starts_with("up to date:"), "{again}");

        fs::write(&exe, b"old-gitcko-bytes").unwrap();
        let err = script_update(&exe, &base, target, "0.0.0", false, false).unwrap_err();
        assert_eq!(err, UpdateError::Swap);
        assert_eq!(fs::read(&exe).unwrap(), b"old-gitcko-bytes");
        assert!(!install.join("gitcko.new").exists());

        #[cfg(unix)]
        {
            let link = install.join("linked");
            std::os::unix::fs::symlink(&exe, &link).unwrap();
            let err = replace_executable(&link, b"nope", true).unwrap_err();
            assert_eq!(err, UpdateError::Swap);
            assert_eq!(fs::read(&exe).unwrap(), b"old-gitcko-bytes");
        }

        tamper_and_expect(
            &release,
            &exe,
            target,
            |sums| {
                let mut sums = sums;
                sums.push(b'\n');
                sums.extend(b"canary_update_secret\n");
                sums
            },
            UpdateError::Verify(VerifyFailure::BadSignature),
        );
    }

    #[test]
    fn wrong_key_missing_signature_and_bad_checksum_each_leave_the_old_binary() {
        let root = tempfile::tempdir().unwrap();
        let target = "aarch64-apple-darwin";
        let release = root.path().join("rel");
        let exe = root.path().join("gitcko");
        fs::write(&exe, b"stay").unwrap();

        write_release(&release, target, b"next", "1.2.3", sign_other);
        let err = script_update(
            &exe,
            &Base::Dir(release.clone()),
            target,
            "0.0.0",
            false,
            true,
        )
        .unwrap_err();
        assert_eq!(err, UpdateError::Verify(VerifyFailure::WrongKey));
        assert_eq!(fs::read(&exe).unwrap(), b"stay");

        copy_release("integrations/install/testdata/release-good", &release);
        fs::write(release.join("SHA256SUMS.minisig"), b"   \n").unwrap();
        let err = script_update(
            &exe,
            &Base::Dir(release.clone()),
            target,
            "0.0.0",
            false,
            true,
        )
        .unwrap_err();
        assert_eq!(err, UpdateError::Verify(VerifyFailure::MissingSignature));
        assert_eq!(fs::read(&exe).unwrap(), b"stay");

        copy_release("integrations/install/testdata/release-good", &release);
        let archive = release.join("gitcko-aarch64-apple-darwin.tar.gz");
        let mut bytes = fs::read(&archive).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        fs::write(&archive, bytes).unwrap();
        let err =
            script_update(&exe, &Base::Dir(release), target, "0.0.0", false, true).unwrap_err();
        assert_eq!(err, UpdateError::Verify(VerifyFailure::Checksum));
        assert_eq!(fs::read(&exe).unwrap(), b"stay");
    }

    #[test]
    #[test]
    fn version_cmp_orders_numeric_segments() {
        use std::cmp::Ordering;
        assert_eq!(super::version_cmp("0.0.1", "2.0.0"), Ordering::Less);
        assert_eq!(super::version_cmp("9.9.9", "2.0.0"), Ordering::Greater);
        assert_eq!(super::version_cmp("2.0.0", "2.0.0"), Ordering::Equal);
    }

    #[test]
    fn canary_a_version_file_is_not_printed_when_it_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let target = "aarch64-apple-darwin";
        let release = root.path().join("rel");
        copy_release(
            "integrations/install/testdata/release-bad-version",
            &release,
        );
        let exe = root.path().join("gitcko");
        fs::write(&exe, b"stay").unwrap();
        let err =
            script_update(&exe, &Base::Dir(release), target, "0.0.0", true, true).unwrap_err();
        assert_eq!(err, UpdateError::Version);
        assert!(!err.detail().contains("canary_"));
        assert_eq!(fs::read(&exe).unwrap(), b"stay");
    }

    #[test]
    fn an_archive_with_a_second_path_is_rejected_and_a_flat_one_extracts() {
        let good = tar_gz(b"flat-bytes");
        assert_eq!(
            extract("gitcko-aarch64-apple-darwin.tar.gz", &good).unwrap(),
            b"flat-bytes"
        );
        let mut header = tar::Header::new_gnu();
        if header.set_path("../gitcko").is_ok() {
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            let raw_tar = {
                let mut builder = tar::Builder::new(Vec::new());
                builder.append(&header, &b"evil"[..]).unwrap();
                builder.finish().unwrap();
                builder.into_inner().unwrap()
            };
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(&raw_tar).unwrap();
            let bad = gz.finish().unwrap();
            assert_eq!(
                extract("gitcko-aarch64-apple-darwin.tar.gz", &bad),
                Err(UpdateError::Archive)
            );
        }

        let zip_bytes = zip_with(b"zip-bytes");
        assert_eq!(
            extract("gitcko-x86_64-pc-windows-msvc.zip", &zip_bytes).unwrap(),
            b"zip-bytes"
        );
    }

    #[test]
    fn doctor_passes_a_signed_script_install_and_fails_the_tampered_ones() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("bin/gitcko");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let bytes = fs::read(repo_path("integrations/install/testdata/doctor.bin")).unwrap();
        fs::write(&exe, &bytes).unwrap();
        assert_eq!(doctor_signature_check(&exe).detail, "not a script install");

        let release = root.path().join("state");
        fs::create_dir_all(&release).unwrap();
        fs::copy(
            repo_path("integrations/install/testdata/doctor.SHA256SUMS"),
            release.join("SHA256SUMS"),
        )
        .unwrap();
        fs::copy(
            repo_path("integrations/install/testdata/doctor.SHA256SUMS.minisig"),
            release.join("SHA256SUMS.minisig"),
        )
        .unwrap();
        fs::write(
            exe.parent().unwrap().join("gitcko-install.txt"),
            format!("method=script\nrelease_dir={}\n", release.display()),
        )
        .unwrap();
        let ok = doctor_signature_check(&exe);
        assert_eq!(ok.status, "pass");
        assert_eq!(ok.detail, "signature ok");

        fs::write(&exe, b"tampered-binary").unwrap();
        let bad = doctor_signature_check(&exe);
        assert_eq!(bad.status, "fail");
        assert_eq!(bad.detail, "binary hash is not in the signed checksums");

        fs::write(&exe, &bytes).unwrap();
        fs::write(
            release.join("SHA256SUMS.minisig"),
            sign_other(&fs::read(release.join("SHA256SUMS")).unwrap()),
        )
        .unwrap();
        let wrong = doctor_signature_check(&exe);
        assert_eq!(wrong.status, "fail");
        assert_eq!(wrong.detail, "signature from a different key");

        fs::write(release.join("SHA256SUMS.minisig"), b"").unwrap();
        assert_eq!(doctor_signature_check(&exe).detail, "signature missing");
    }

    #[test]
    fn commands_print_the_install_method_and_npm_check_does_not_run_npm() {
        let root = tempfile::tempdir().unwrap();
        let io = Io::capture();
        let env = BTreeMap::new();
        let exe = root.path().join("copied/gitcko");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"x").unwrap();
        let code = run(&UpdateArgs { check: false }, Some(&exe), &env, &io);
        assert_eq!(code, 1);
        assert!(io.stderr_text().contains("cannot be updated"));
        assert!(io.stdout_text().is_empty());

        let brew = root.path().join("homebrew/Cellar/gitcko/1/bin/gitcko");
        fs::create_dir_all(brew.parent().unwrap()).unwrap();
        fs::write(&brew, b"x").unwrap();
        let io = Io::capture();
        assert_eq!(run(&UpdateArgs { check: true }, Some(&brew), &env, &io), 0);
        assert_eq!(io.stdout_text().trim(), "brew upgrade gitcko");

        let npm = root.path().join("node_modules/gitcko/gitcko");
        fs::create_dir_all(npm.parent().unwrap()).unwrap();
        fs::write(&npm, b"x").unwrap();
        let log = root.path().join("npm.log");
        let mut env = BTreeMap::from([
            ("GITCKO_NPM_LOG".into(), log.display().to_string()),
            (
                "GITCKO_DOWNLOAD_URL".into(),
                "http://example.com/canary_update_secret".into(),
            ),
        ]);
        let bin = root.path().join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        write_fake_npm(&bin, &log);
        env.insert("PATH".into(), bin.display().to_string());
        let io = Io::capture();
        assert_eq!(run(&UpdateArgs { check: true }, Some(&npm), &env, &io), 0);
        assert_eq!(io.stdout_text().trim(), "npm i -g gitcko@latest");
        assert!(!log.exists(), "check must not run npm");
        let io = Io::capture();
        assert_eq!(run(&UpdateArgs { check: false }, Some(&npm), &env, &io), 0);
        let ran = fs::read_to_string(&log).unwrap();
        assert!(ran.contains("gitcko@latest"), "{ran}");
        assert!(!ran.contains("canary_update_secret"), "{ran}");
    }

    fn tamper_and_expect(
        release: &Path,
        exe: &Path,
        target: &str,
        tamper: impl FnOnce(Vec<u8>) -> Vec<u8>,
        expect: UpdateError,
    ) {
        let path = release.join("SHA256SUMS");
        let sums = fs::read(&path).unwrap();
        fs::write(&path, tamper(sums)).unwrap();
        fs::write(exe, b"old-gitcko-bytes").unwrap();
        let err = script_update(
            exe,
            &Base::Dir(release.to_path_buf()),
            target,
            "0.0.0",
            false,
            true,
        )
        .unwrap_err();
        assert_eq!(err, expect);
        assert_eq!(fs::read(exe).unwrap(), b"old-gitcko-bytes");
    }

    fn copy_release(from: &str, dest: &Path) {
        let src = repo_path(from);
        fs::create_dir_all(dest).unwrap();
        for name in [
            "SHA256SUMS",
            "SHA256SUMS.minisig",
            "VERSION",
            "gitcko-aarch64-apple-darwin.tar.gz",
        ] {
            fs::copy(src.join(name), dest.join(name)).unwrap();
        }
    }

    fn write_release(
        dir: &Path,
        target: &str,
        binary: &[u8],
        version: &str,
        sign: fn(&[u8]) -> String,
    ) {
        fs::create_dir_all(dir).unwrap();
        let archive_name = format!("gitcko-{target}.tar.gz");
        let bin_name = format!("gitcko-{target}.bin");
        let archive = tar_gz(binary);
        fs::write(dir.join(&archive_name), &archive).unwrap();
        fs::write(dir.join("VERSION"), format!("{version}\n")).unwrap();
        let sums = format!(
            "{archive_hash}  {archive_name}\n{bin_hash}  {bin_name}\n{version_hash}  VERSION\n",
            archive_hash = hex(&sha256(&archive)),
            bin_hash = hex(&sha256(binary)),
            version_hash = hex(&sha256(format!("{version}\n").as_bytes())),
        );
        fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        fs::write(dir.join("SHA256SUMS.minisig"), sign(sums.as_bytes())).unwrap();
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn tar_gz(binary: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_path("gitcko").unwrap();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            builder.append(&header, binary).unwrap();
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&raw).unwrap();
        gz.finish().unwrap()
    }

    fn zip_with(binary: &[u8]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("gitcko.exe", options).unwrap();
            zip.write_all(binary).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn sign_other(message: &[u8]) -> String {
        let pair = minisign::KeyPair::generate_encrypted_keypair(Some("other-key".into())).unwrap();
        minisign::sign(None, &pair.sk, Cursor::new(message), Some("other"), None)
            .unwrap()
            .into_string()
    }

    fn repo_path(rel: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.pop();
        path.push(rel);
        path
    }

    fn write_fake_npm(dir: &Path, log: &Path) {
        let log = log.display().to_string();
        #[cfg(unix)]
        {
            let path = dir.join("npm");
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" \"${{GITCKO_DOWNLOAD_URL-}}\" > '{log}'\n"
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }
        #[cfg(windows)]
        {
            let path = dir.join("npm.cmd");
            fs::write(
                &path,
                format!("@echo off\r\necho %* %GITCKO_DOWNLOAD_URL% > \"{log}\"\r\n"),
            )
            .unwrap();
        }
    }

    fn serve(status: u16, extra_headers: &str, body: &[u8]) -> String {
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let _ = NEXT.fetch_add(1, Ordering::Relaxed);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = body.to_vec();
        let extra = extra_headers.to_string();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf);
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.write_all(&body);
        });
        format!("http://127.0.0.1:{port}")
    }
}
