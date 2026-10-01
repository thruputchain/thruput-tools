use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use flate2::Compression;
use flate2::write::GzEncoder;
use groundhog_toolchain::{
    Artifact, BundleSource, Catalog, DEFAULT_PUBLIC_ARTIFACT_BASE_URL, HostTarget, HttpSource,
    InstallError, Installer, SourceError,
};
use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};

struct MemorySource {
    bytes: Vec<u8>,
    calls: AtomicUsize,
}

impl MemorySource {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl BundleSource for MemorySource {
    async fn fetch_to(&self, _artifact: &Artifact, destination: &Path) -> Result<u64, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::write(destination, &self.bytes).map_err(|source| SourceError::Write {
            path: destination.to_path_buf(),
            source,
        })?;
        Ok(self.bytes.len() as u64)
    }
}

#[tokio::test]
async fn install_verifies_extracts_and_reuses_a_complete_bundle() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let first = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect("bug: fixture bundle should install");
    let second = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect("bug: complete bundle should resolve");

    assert!(first.installed_now());
    assert!(!second.installed_now());
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(first.binary_path()).expect("bug: binary should read"),
        b"binary"
    );
    assert_eq!(
        std::fs::read(first.path().join("contracts/dev_wallet.rv32a"))
            .expect("bug: wallet should read"),
        b"wallet"
    );
    assert!(first.path().join(".ok").is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(first.binary_path())
            .expect("bug: binary metadata should read")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[tokio::test]
async fn install_rejects_a_missing_genesis_contract() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .unwrap_err();
    assert!(matches!(error, InstallError::MissingArchiveMember { path }
        if path == Path::new("contracts/genesis_root.rv32a")));
}

#[test]
fn cancelled_lock_wait_does_not_block_runtime_shutdown() {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    use std::time::Duration;

    let cache = tempfile::tempdir().unwrap();
    let locks = cache.path().join("toolchain/hog-devnode/.locks");
    std::fs::create_dir_all(&locks).unwrap();
    let lock_path = locks.join("0.1.13-x86_64-unknown-linux-gnu.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    FileExt::lock_exclusive(&lock).unwrap();
    let installer = Installer::new(cache.path());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(100),
                    installer.lease(&catalog_for(b"fixture"), "0.1.13", HostTarget::LinuxX86_64)
                )
                .await
                .is_err()
            );
        });
        drop(runtime);
        done_tx.send(()).unwrap();
    });
    let shutdown = done_rx.recv_timeout(Duration::from_secs(3));
    // Release the fixture lock even on failure so a regressed worker can finish.
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    assert!(
        shutdown.is_ok(),
        "cancelled lock wait blocked runtime shutdown"
    );
}

#[test]
fn clean_removes_a_version_no_longer_in_the_catalog() {
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let retired = installer.installation_path("0.0.1", HostTarget::LinuxX86_64);
    std::fs::create_dir_all(&retired).unwrap();
    std::fs::write(retired.join("hog-devnode"), b"old").unwrap();
    assert!(installer.clean("0.0.1", HostTarget::LinuxX86_64).unwrap());
    assert!(!retired.exists());
    for version in [".", "..", "../escape", "/absolute", ""] {
        assert!(matches!(
            installer.clean(version, HostTarget::LinuxX86_64),
            Err(InstallError::Catalog(
                groundhog_toolchain::CatalogError::InvalidVersion { .. }
            ))
        ));
    }
}

#[tokio::test]
async fn install_rejects_checksum_mismatch_without_completing() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let mut corrupt = archive;
    corrupt[128] ^= 0x01;
    let source = MemorySource::new(corrupt);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: checksum mismatch should fail");

    assert!(matches!(error, InstallError::ChecksumMismatch { .. }));
    assert!(
        !installer
            .installation_path("0.1.13", HostTarget::LinuxX86_64)
            .exists()
    );
}

#[tokio::test]
async fn install_rejects_undeclared_regular_members() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
        ("unexpected", b"payload"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: undeclared archive member should fail");

    assert!(matches!(
        error,
        InstallError::UndeclaredArchiveMember { .. }
    ));
}

#[tokio::test]
async fn install_accepts_zero_length_declared_parent_directories() {
    let archive = archive_with_directory("contracts/");
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let installation = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect("bug: declared parent directory should be accepted");

    assert!(
        installation
            .path()
            .join("contracts/dev_wallet.rv32a")
            .is_file()
    );
}

#[tokio::test]
async fn install_rejects_undeclared_directory_entries() {
    let archive = archive_with_directory("unexpected/");
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: undeclared directory should fail");

    assert!(matches!(
        error,
        InstallError::UndeclaredArchiveMember { .. }
    ));
}

#[tokio::test]
async fn install_rejects_symlinks_even_for_declared_paths() {
    let archive = archive_with_symlink();
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: symlink archive member should fail");

    assert!(matches!(error, InstallError::ArchiveMemberType { .. }));
}

#[tokio::test]
async fn concurrent_installs_download_once_and_share_the_completed_bundle() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let (first, second) = tokio::join!(
        installer.install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source),
        installer.install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
    );

    let first = first.expect("bug: first concurrent install should succeed");
    let second = second.expect("bug: second concurrent install should succeed");
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_ne!(first.installed_now(), second.installed_now());
    assert_eq!(first.path(), second.path());
}

#[tokio::test]
async fn incomplete_installation_is_replaced_under_the_lock() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());
    let incomplete = installer.installation_path("0.1.13", HostTarget::LinuxX86_64);
    std::fs::create_dir_all(&incomplete).expect("bug: incomplete directory should create");
    std::fs::write(incomplete.join("partial"), b"stale")
        .expect("bug: incomplete file should write");

    let installation = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect("bug: incomplete installation should be replaced");

    assert!(installation.installed_now());
    assert!(!installation.path().join("partial").exists());
    assert!(installation.path().join(".ok").is_file());
}

#[tokio::test]
async fn clean_removes_a_complete_installation_and_is_idempotent() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());
    installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect("bug: fixture bundle should install");

    assert!(
        installer
            .clean("0.1.13", HostTarget::LinuxX86_64)
            .expect("bug: clean should succeed")
    );
    assert!(
        installer
            .find(&catalog, "0.1.13", HostTarget::LinuxX86_64)
            .expect("bug: local lookup should succeed")
            .is_none()
    );
    assert!(
        !installer
            .clean("0.1.13", HostTarget::LinuxX86_64)
            .expect("bug: repeated clean should succeed")
    );
}

#[tokio::test]
async fn install_rejects_hardlinks_even_for_declared_paths() {
    let archive = archive_with_link(EntryType::Link);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: hardlink archive member should fail");

    assert!(matches!(error, InstallError::ArchiveMemberType { .. }));
}

#[tokio::test]
async fn install_rejects_parent_traversal_before_materializing_a_file() {
    let archive = archive_with_patched_first_header("../escape", None);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: parent traversal should fail");

    assert!(matches!(error, InstallError::UnsafeArchiveMember { .. }));
    assert!(!cache.path().join("escape").exists());
}

#[tokio::test]
async fn install_rejects_a_member_size_above_the_expansion_bound() {
    let archive = archive_with_patched_first_header(
        "contracts/dev_wallet.rv32a",
        Some(256 * 1024 * 1024 + 1),
    );
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
        .await
        .expect_err("bug: oversized member should fail before expansion");

    assert!(matches!(error, InstallError::ArchiveMemberSize { .. }));
}

#[tokio::test]
async fn install_rejects_more_than_the_bounded_member_count() {
    let mut files = vec![
        ("contracts/dev_wallet.rv32a".to_string(), b"wallet".to_vec()),
        (
            "contracts/genesis_root.rv32a".to_string(),
            b"genesis".to_vec(),
        ),
        ("hog-devnode".to_string(), b"binary".to_vec()),
    ];
    for index in 0u8..63 {
        files.push((format!("lib/libfixture{index}.dylib"), vec![index]));
    }
    let archive = owned_regular_archive(&files);
    let catalog = catalog_for_macos_files(&archive, &files);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let error = installer
        .install(&catalog, "0.1.13", HostTarget::MacosAarch64, &source)
        .await
        .expect_err("bug: excessive member count should fail");

    assert!(matches!(error, InstallError::ArchiveMemberCount { .. }));
}

#[tokio::test]
async fn http_source_rejects_an_advertised_oversized_body_before_writing() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let artifact = catalog
        .artifact("0.1.13", HostTarget::LinuxX86_64)
        .expect("bug: fixture artifact should resolve");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: fixture server should bind");
    let address = listener
        .local_addr()
        .expect("bug: fixture address should resolve");
    let response_size = artifact.size() + 1;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener
            .accept()
            .await
            .expect("bug: fixture request should connect");
        let mut request = [0u8; 4096];
        let _ = stream
            .read(&mut request)
            .await
            .expect("bug: fixture request should read");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {response_size}\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("bug: fixture response should write");
    });
    let source =
        HttpSource::new(format!("http://{address}")).expect("bug: fixture source should construct");
    let download = tempfile::NamedTempFile::new().expect("bug: download temp file should create");

    let error = source
        .fetch_to(artifact, download.path())
        .await
        .expect_err("bug: oversized response should fail");
    server.await.expect("bug: fixture server should finish");

    assert!(matches!(error, SourceError::SizeMismatch { .. }));
    assert_eq!(
        std::fs::metadata(download.path())
            .expect("bug: download metadata should read")
            .len(),
        0
    );
}

#[tokio::test]
#[ignore = "requires access to the public artifact bucket"]
async fn public_bundle_installs_with_the_embedded_catalog() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");
    let target = HostTarget::current().expect("bug: smoke must run on a managed target");
    let source = HttpSource::new(DEFAULT_PUBLIC_ARTIFACT_BASE_URL)
        .expect("bug: public source should construct");
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    let installation = installer
        .install(&catalog, catalog.default_version(), target, &source)
        .await
        .expect("bug: published bundle should verify and install");

    assert!(installation.binary_path().is_file());
    assert!(
        installation
            .path()
            .join("contracts/dev_wallet.rv32a")
            .is_file()
    );
    assert!(installation.path().join(".ok").is_file());
}

// Fixture archives exercise the parser independently of the currently published release.
fn fixture_catalog() -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_slice(Catalog::embedded_bytes())
        .expect("bug: embedded catalog should be JSON");
    let current = value["defaultVersion"].as_str().unwrap().to_string();
    let mut release = value["versions"][&current].clone();
    release["source"]["ref"] = serde_json::json!("v0.1.13");
    for (triple, target) in release["targets"].as_object_mut().unwrap() {
        let archive = format!("hog-devnode_0.1.13_{triple}.tar.gz");
        target["archive"] = serde_json::json!(archive);
        target["objectKey"] = serde_json::json!(format!("hog-devnode/0.1.13/{archive}"));
    }
    value["defaultVersion"] = serde_json::json!("0.1.13");
    value["versions"] = serde_json::json!({"0.1.13": release});
    value
}

fn catalog_for(archive: &[u8]) -> Catalog {
    let mut value = fixture_catalog();
    let target = &mut value["versions"]["0.1.13"]["targets"]["x86_64-unknown-linux-gnu"];
    target["size"] = serde_json::Value::from(archive.len() as u64);
    target["sha256"] = serde_json::Value::String(hex::encode(Sha256::digest(archive)));
    let bytes = serde_json::to_vec(&value).expect("bug: test catalog should serialize");
    Catalog::parse(&bytes).expect("bug: test catalog should validate")
}

fn catalog_for_macos_files(archive: &[u8], files: &[(String, Vec<u8>)]) -> Catalog {
    let mut value = fixture_catalog();
    let target = &mut value["versions"]["0.1.13"]["targets"]["aarch64-apple-darwin"];
    target["size"] = serde_json::Value::from(archive.len() as u64);
    target["sha256"] = serde_json::Value::String(hex::encode(Sha256::digest(archive)));
    target["files"] = serde_json::Value::Array(
        files
            .iter()
            .map(|(path, _)| {
                let kind = match path.as_str() {
                    "hog-devnode" => "executable",
                    "contracts/dev_wallet.rv32a" | "contracts/genesis_root.rv32a" => "artifact",
                    _ => "runtimeLibrary",
                };
                serde_json::json!({"kind": kind, "path": path})
            })
            .collect(),
    );
    let bytes = serde_json::to_vec(&value).expect("bug: test catalog should serialize");
    Catalog::parse(&bytes).expect("bug: test catalog should validate")
}

fn regular_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    gzip(&regular_tar(files))
}

fn regular_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o777);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *bytes)
            .expect("bug: tar member should append");
    }
    builder.into_inner().expect("bug: tar should finish")
}

fn owned_regular_archive(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o777);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, path, bytes.as_slice())
            .expect("bug: tar member should append");
    }
    let archive = builder.into_inner().expect("bug: tar should finish");
    gzip(&archive)
}

fn archive_with_symlink() -> Vec<u8> {
    archive_with_link(EntryType::Symlink)
}

fn archive_with_link(entry_type: EntryType) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = Builder::new(encoder);

    let mut wallet = Header::new_gnu();
    wallet.set_entry_type(EntryType::Regular);
    wallet.set_mode(0o644);
    wallet.set_size(6);
    wallet.set_cksum();
    builder
        .append_data(&mut wallet, "contracts/dev_wallet.rv32a", &b"wallet"[..])
        .expect("bug: wallet should append");

    let mut link = Header::new_gnu();
    link.set_entry_type(entry_type);
    link.set_mode(0o777);
    link.set_size(0);
    link.set_link_name("/tmp/not-hog-devnode")
        .expect("bug: link target should set");
    link.set_cksum();
    builder
        .append_data(&mut link, "hog-devnode", &[][..])
        .expect("bug: symlink should append");

    let encoder = builder.into_inner().expect("bug: tar should finish");
    encoder.finish().expect("bug: gzip should finish")
}

fn archive_with_directory(directory: &str) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = Builder::new(encoder);
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Directory);
    header.set_mode(0o755);
    header.set_size(0);
    header.set_cksum();
    builder
        .append_data(&mut header, directory, &[][..])
        .expect("bug: directory should append");
    for (path, bytes) in [
        ("contracts/dev_wallet.rv32a", &b"wallet"[..]),
        ("contracts/genesis_root.rv32a", &b"genesis"[..]),
        ("hog-devnode", &b"binary"[..]),
    ] {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o777);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, path, bytes)
            .expect("bug: tar member should append");
    }
    let encoder = builder.into_inner().expect("bug: tar should finish");
    encoder.finish().expect("bug: gzip should finish")
}

fn archive_with_patched_first_header(path: &str, size: Option<u64>) -> Vec<u8> {
    let mut archive = regular_tar(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    archive[..100].fill(0);
    archive[..path.len()].copy_from_slice(path.as_bytes());
    if let Some(size) = size {
        archive[124..136].fill(0);
        let field = format!("{size:011o}\0");
        archive[124..136].copy_from_slice(field.as_bytes());
    }
    archive[148..156].fill(b' ');
    let checksum: u64 = archive[..512].iter().map(|byte| u64::from(*byte)).sum();
    let checksum_field = format!("{checksum:06o}\0 ");
    archive[148..156].copy_from_slice(checksum_field.as_bytes());
    gzip(&archive)
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(bytes)
        .expect("bug: gzip payload should write");
    encoder.finish().expect("bug: gzip should finish")
}

/// Release gate: exercise the actual consumer against every default public bundle.
#[tokio::test]
#[ignore = "requires access to the public artifact bucket"]
async fn public_catalog_installs_all_targets() {
    let catalog = Catalog::embedded().expect("bug: embedded catalog should validate");
    let source = HttpSource::new(DEFAULT_PUBLIC_ARTIFACT_BASE_URL)
        .expect("bug: public source should construct");
    let cache = tempfile::tempdir().expect("bug: cache tempdir should create");
    let installer = Installer::new(cache.path());

    for target in HostTarget::ALL {
        let installation = installer
            .install(&catalog, catalog.default_version(), target, &source)
            .await
            .unwrap_or_else(|error| panic!("public bundle for {target:?} failed: {error}"));
        assert!(installation.installed_now());
        let artifact = catalog
            .artifact(catalog.default_version(), target)
            .expect("bug: default target should exist");
        for file in artifact.files() {
            assert!(
                installation.path().join(file.path()).is_file(),
                "missing {} for {target:?}",
                file.path()
            );
        }
        assert!(installation.path().join(".ok").is_file());
    }
}

#[tokio::test]
async fn leases_share_a_bundle_and_prevent_cleanup_until_last_user_stops() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let target = HostTarget::LinuxX86_64;
    assert!(
        installer
            .lease(&catalog, "0.1.13", target)
            .await
            .unwrap()
            .is_none()
    );
    installer
        .install(&catalog, "0.1.13", target, &source)
        .await
        .unwrap();
    let first = installer
        .lease(&catalog, "0.1.13", target)
        .await
        .unwrap()
        .unwrap();
    let second = installer
        .lease(&catalog, "0.1.13", target)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        installer.clean("0.1.13", target),
        Err(InstallError::Busy { .. })
    ));
    // Prefetch/reuse must not wait for running nodes or download a second copy.
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        installer.install(&catalog, "0.1.13", target, &source),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    drop(first);
    assert!(matches!(
        installer.clean("0.1.13", target),
        Err(InstallError::Busy { .. })
    ));
    assert_eq!(std::fs::read(second.binary_path()).unwrap(), b"binary");
    drop(second);
    assert!(installer.clean("0.1.13", target).unwrap());
    assert!(
        installer
            .lease(&catalog, "0.1.13", target)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn waiting_installer_reuses_a_bundle_even_when_a_node_takes_a_lease() {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    use std::time::Duration;

    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let seed = Installer::new(cache.path().join("seed"));
    let target = HostTarget::LinuxX86_64;
    let completed = seed
        .install(&catalog, "0.1.13", target, &source)
        .await
        .unwrap();
    let locks = cache.path().join("toolchain/hog-devnode/.locks");
    std::fs::create_dir_all(&locks).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(locks.join("0.1.13-x86_64-unknown-linux-gnu.lock"))
        .unwrap();
    FileExt::lock_shared(&lock).unwrap();
    let install = installer.install(&catalog, "0.1.13", target, &source);
    tokio::pin!(install);
    // Poll through the missing-cache check into exclusive lock contention.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut install)
            .await
            .is_err()
    );
    let destination = installer.installation_path("0.1.13", target);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    // Model another installer committing, followed immediately by a node's shared lease.
    // Keeping this shared lock held proves the waiter no longer requires exclusive access.
    std::fs::rename(completed.path(), &destination).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut install)
        .await
        .unwrap()
        .unwrap();
    assert!(!result.installed_now());
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    FileExt::unlock(&lock).unwrap();
}

#[tokio::test]
async fn install_repairs_non_directory_cache_entries() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let target = HostTarget::LinuxX86_64;
    let path = installer.installation_path("0.1.13", target);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"stray file").unwrap();
    assert!(
        installer
            .find(&catalog, "0.1.13", target)
            .unwrap()
            .is_none()
    );
    installer
        .install(&catalog, "0.1.13", target, &source)
        .await
        .unwrap();
    assert_eq!(std::fs::read(path.join("hog-devnode")).unwrap(), b"binary");
    installer.clean("0.1.13", target).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(cache.path().join("missing"), &path).unwrap();
        assert!(
            installer
                .find(&catalog, "0.1.13", target)
                .unwrap()
                .is_none()
        );
        installer
            .install(&catalog, "0.1.13", target, &source)
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(&path).unwrap().is_dir());
    }
}

#[test]
fn cleanup_sweeps_only_its_targets_abandoned_work() {
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let version_dir = cache.path().join("toolchain/hog-devnode/0.1.13");
    let ours = version_dir.join(".staging-x86_64-unknown-linux-gnu-orphan");
    let theirs = version_dir.join(".staging-aarch64-unknown-linux-gnu-active");
    std::fs::create_dir_all(&ours).unwrap();
    std::fs::create_dir_all(&theirs).unwrap();
    let download = version_dir.join(".download-x86_64-unknown-linux-gnu-orphan");
    std::fs::write(&download, b"partial").unwrap();
    assert!(installer.clean("0.1.13", HostTarget::LinuxX86_64).unwrap());
    assert!(!ours.exists());
    assert!(!download.exists());
    assert!(theirs.is_dir());
}

#[tokio::test]
async fn archive_extensions_and_dot_components_are_rejected_explicitly() {
    for (archive, extension) in [
        (archive_with_link(EntryType::XHeader), true),
        (
            archive_with_patched_first_header("contracts/./dev_wallet.rv32a", None),
            false,
        ),
    ] {
        let catalog = catalog_for(&archive);
        let source = MemorySource::new(archive);
        let cache = tempfile::tempdir().unwrap();
        let result = Installer::new(cache.path())
            .install(&catalog, "0.1.13", HostTarget::LinuxX86_64, &source)
            .await;
        if extension {
            assert!(
                matches!(
                    result,
                    Err(InstallError::ArchiveMemberType {
                        entry_type: b'x',
                        ..
                    })
                ),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(InstallError::UnsafeArchiveMember { .. })),
                "{result:?}"
            );
        }
    }
}

#[test]
fn cancelled_install_does_not_block_the_runtime_or_commit_queued_work() {
    let archive = regular_archive(&[
        ("contracts/dev_wallet.rv32a", b"wallet"),
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let catalog = catalog_for(&archive);
    let source = MemorySource::new(archive);
    let cache = tempfile::tempdir().unwrap();
    let installer = Installer::new(cache.path());
    let target = HostTarget::LinuxX86_64;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // Saturate the worker pool. A runtime-thread tail would incorrectly finish
        // before this timer; a queued worker must remain cancellable and uncommitted.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            installer.install(&catalog, "0.1.13", target, &source),
        )
        .await;
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        assert!(
            result.is_err(),
            "installation tail ran on the async runtime"
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match installer.clean("0.1.13", target) {
                    Ok(removed) => {
                        assert!(!removed, "cancelled worker committed or leaked staging");
                        break;
                    }
                    Err(InstallError::Busy { .. }) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("unexpected cleanup error: {error}"),
                }
            }
        })
        .await
        .unwrap();
        assert!(
            installer
                .find(&catalog, "0.1.13", target)
                .unwrap()
                .is_none()
        );
    });
}

#[tokio::test]
async fn native_bundle_installs_without_legacy_wallet_artifact() {
    let archive = regular_archive(&[
        ("contracts/genesis_root.rv32a", b"genesis"),
        ("hog-devnode", b"binary"),
    ]);
    let mut value = fixture_catalog();
    let target = &mut value["versions"]["0.1.13"]["targets"]["x86_64-unknown-linux-gnu"];
    target["size"] = serde_json::json!(archive.len());
    target["sha256"] = serde_json::json!(hex::encode(Sha256::digest(&archive)));
    target["files"]
        .as_array_mut()
        .unwrap()
        .retain(|file| file["path"] != "contracts/dev_wallet.rv32a");
    let catalog = Catalog::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let installation = Installer::new(cache.path())
        .install(
            &catalog,
            "0.1.13",
            HostTarget::LinuxX86_64,
            &MemorySource::new(archive),
        )
        .await
        .unwrap();
    assert!(installation.binary_path().is_file());
    assert!(
        installation
            .path()
            .join("contracts/genesis_root.rv32a")
            .is_file()
    );
    assert!(
        !installation
            .path()
            .join("contracts/dev_wallet.rv32a")
            .exists()
    );
}
