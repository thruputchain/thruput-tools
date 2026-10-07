use crate::artifacts::{self, ArtifactKind, ArtifactOptions, ManifestFlags};
use crate::checks::{Finding, FindingLevel};
use crate::{CliError, Print};
use groundhog_contract_spec::ContractSpec;
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) const RV32_TARGET_TRIPLE: &str = "riscv32im-unknown-none-elf";

/// Schema version of the `build --json` data payload.
pub const BUILD_JSON_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub project_name: String,
    pub project_root: PathBuf,
    /// Manifest directory of the built package. In a single-package project
    /// this equals `project_root`; in a workspace it is the member's own
    /// directory (e.g. `<workspace>/erc20`).
    #[serde(skip_serializing)]
    pub package_root: PathBuf,
    #[serde(skip_serializing)]
    pub target_directory: PathBuf,
    pub elf: PathBuf,
    pub rv32: PathBuf,
    pub metadata: PathBuf,
    pub spec_json: Option<PathBuf>,
    pub spec_xdr: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bindings_ts: Option<PathBuf>,
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    pub size_bytes: u64,
    /// Bytes in the artifact's code section.
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    pub code_bytes: usize,
    /// Guest data address space the artifact declares (`memory_bytes` in its
    /// header).
    #[serde(with = "groundhog_json_rpc_types::decimal")]
    pub memory_bytes: usize,
    pub target: ArtifactKind,
    pub profile: crate::config::ResolvedProfile,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cargo_stderr: String,
    /// Findings `build --json` lists in its envelope's `warnings`.
    #[serde(skip_serializing)]
    pub warnings: Vec<Finding>,
}

/// Build the contract to an RV32 artifact.
pub fn run(
    package: Option<&str>,
    bindings: &[String],
    stack_size: Option<u32>,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    print: &Print,
) -> Result<(), CliError> {
    let build_print = print
        .clone()
        .with_compact_build(inline_build_args(cargo_args));
    let task = crate::print::progress::Task::start(
        &build_print,
        "Inspecting project",
        inline_build_args(cargo_args),
    );
    let result = build_rv32_project(
        package,
        cargo_args,
        artifacts,
        explicit_profile,
        stack_size,
        bindings,
        task.print(),
    );
    let compact = task.print().is_compact_build();
    let warnings = task.finish();
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            for warning in warnings {
                if compact {
                    print.defer_warning(warning);
                } else {
                    print.warn(warning);
                }
            }
            return Err(error);
        }
    };
    print.outcome_report(
        crate::print::Outcome::Success,
        "Build complete",
        &build_summary(&result),
        &warnings,
    );
    print.json(&crate::result::CommandResult::with_warnings(
        "build",
        result.warnings,
        json!({
            "schemaVersion": BUILD_JSON_SCHEMA_VERSION,
            "projectName": result.project_name,
            "projectRoot": result.project_root,
            "target": result.target,
            "elf": result.elf,
            "rv32": result.rv32,
            "metadata": result.metadata,
            "specJson": result.spec_json,
            "specXdr": result.spec_xdr,
            "bindingsTs": result.bindings_ts,
            "sizeBytes": result.size_bytes.to_string(),
            "codeBytes": result.code_bytes.to_string(),
            "memoryBytes": result.memory_bytes.to_string(),
            "profile": result.profile,
            "cargoStdout": result.cargo_stdout,
            "cargoStderr": result.cargo_stderr,
        }),
    ))?;
    Ok(())
}

fn inline_build_args(args: &[String]) -> bool {
    // Preserve explicitly requested Cargo data/style formats and direct I/O modes.
    !args.iter().any(|arg| {
        arg == "--message-format"
            || arg == "--verbose"
            || arg == "-v"
            || arg == "-vv"
            || arg.starts_with("--message-format=")
            || arg == "--color"
            || arg.starts_with("--color=")
    }) && std::env::var_os("CARGO_TERM_COLOR").is_none()
}

fn build_summary(result: &BuildResult) -> Vec<(String, String)> {
    let mut items = vec![
        ("Package".into(), result.project_name.clone()),
        ("Artifact".into(), result.rv32.display().to_string()),
        ("ELF".into(), result.elf.display().to_string()),
        ("Metadata".into(), result.metadata.display().to_string()),
        (
            "Artifact size".into(),
            format!("{} bytes", result.size_bytes),
        ),
        ("Code".into(), format!("{} bytes", result.code_bytes)),
        (
            "Guest memory".into(),
            format!("{} bytes", result.memory_bytes),
        ),
    ];
    for (label, path) in [
        ("Spec JSON", &result.spec_json),
        ("Spec XDR", &result.spec_xdr),
        ("TypeScript", &result.bindings_ts),
    ] {
        if let Some(path) = path {
            items.push((label.into(), path.display().to_string()));
        }
    }
    items
}

/// Refuse to build when the environment carries a rustflags override.
///
/// Cargo lets `CARGO_ENCODED_RUSTFLAGS` / `RUSTFLAGS` — even set-but-empty —
/// replace the project's `.cargo/config.toml` target rustflags wholesale.
/// That drops the Groundhog link args and per-crate codegen flags such as
/// `-Z trap-unreachable=false`, which surfaces later as link failures or
/// artifact-validation rejections instead of an obvious build error.
fn ensure_no_env_rustflags() -> Result<(), CliError> {
    for var in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"] {
        if std::env::var_os(var).is_some() {
            return Err(CliError::EnvRustflagsSet(var));
        }
    }
    Ok(())
}

/// Parse and validate a `--stack-size` value: bytes as decimal, `0x` hex, or a
/// `K`/`M`/`G` suffixed size such as `32K`.
///
/// The value becomes the guest's `__stack_size` linker symbol, so it carries
/// the SDK linker-script contract: a positive multiple of 16. That it leaves
/// room below the guest data address space is checked once that space is
/// known, since the package's manifest can enlarge it.
pub fn parse_stack_size(value: &str) -> Result<u32, StackSizeError> {
    let parsed = crate::byte_size::parse_byte_size(value)?;
    if parsed == 0 || !parsed.is_multiple_of(16) {
        return Err(StackSizeError::Misaligned { bytes: parsed });
    }
    u32::try_from(parsed).map_err(|_| StackSizeError::TooLarge { bytes: parsed })
}

/// Why a `--stack-size` value was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StackSizeError {
    /// The value is not a byte count.
    #[error(transparent)]
    ByteSize(#[from] crate::byte_size::ByteSizeError),
    /// The SDK linker script needs a positive multiple of 16.
    #[error("stack size {bytes} must be a positive multiple of 16")]
    Misaligned {
        /// The requested reserve.
        bytes: u64,
    },
    /// The reserve does not fit the guest's address space.
    #[error("stack size {bytes} does not fit a 32-bit address space")]
    TooLarge {
        /// The requested reserve.
        bytes: u64,
    },
}

/// The `[package.metadata.groundhog]` key that sizes a contract's guest data
/// memory.
pub const INITIAL_MEMORY_BYTES_KEY: &str = "initial_memory_bytes";

/// A byte count as `[package.metadata.groundhog]` may spell it: an integer,
/// or a string in the notation `--stack-size` takes (`"128K"`, `"0x20000"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(untagged)]
pub enum ByteSizeValue {
    /// A plain TOML integer.
    Integer(i64),
    /// A string with an optional `0x` prefix or `K`/`M`/`G` suffix.
    Text(String),
}

/// Parse `[package.metadata.groundhog] initial_memory_bytes`: the guest data
/// address space a contract declares, a positive byte count that fits the
/// artifact header's 32-bit `memory_bytes` field.
pub fn parse_initial_memory_bytes(value: &ByteSizeValue) -> Result<u32, InitialMemoryError> {
    let bytes = match value {
        ByteSizeValue::Integer(bytes) => {
            u64::try_from(*bytes).map_err(|_| InitialMemoryError::Negative { value: *bytes })?
        }
        ByteSizeValue::Text(text) => crate::byte_size::parse_byte_size(text)?,
    };
    if bytes == 0 {
        return Err(InitialMemoryError::Zero);
    }
    u32::try_from(bytes).map_err(|_| InitialMemoryError::TooLarge { bytes })
}

/// Why a package's `initial_memory_bytes` was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitialMemoryError {
    /// The string form is not a byte count.
    #[error(transparent)]
    ByteSize(#[from] crate::byte_size::ByteSizeError),
    /// The manifest states a negative integer.
    #[error("{value} is not a positive byte count")]
    Negative {
        /// The integer the manifest states.
        value: i64,
    },
    /// A contract cannot declare an empty data address space.
    #[error("0 is not a positive byte count")]
    Zero,
    /// The declared space does not fit the artifact header.
    #[error("{bytes} does not fit the artifact's 32-bit memory_bytes field")]
    TooLarge {
        /// The declared space.
        bytes: u64,
    },
}

/// The guest data-memory layout a build links and stamps.
///
/// The stack reserve comes from `--stack-size` and defaults to the SDK linker
/// script's. The data address space comes from the package's
/// `[package.metadata.groundhog] initial_memory_bytes` and defaults to the
/// protocol's 64 KiB. It lives in the manifest rather than on the command
/// line because it is a property of the contract: it changes the artifact,
/// and `groundhog build`, `groundhog test`, and the build behind a deploy
/// must all see the same value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GuestLayout {
    /// The `__stack_size` linker symbol; `None` keeps the linker script's.
    stack_size: Option<u32>,
    /// The `__memory_size` linker symbol and the artifact's `memory_bytes`;
    /// `None` keeps the protocol default.
    initial_memory_bytes: Option<u32>,
}

impl GuestLayout {
    /// The data address space stamped into the artifact, in bytes.
    fn memory_bytes(self) -> usize {
        self.initial_memory_bytes
            .map_or(groundhog_rv32_artifact::DEFAULT_MEMORY_BYTES, |bytes| {
                bytes as usize
            })
    }

    /// Whether the build has to go through `cargo rustc` to pass a `--defsym`.
    fn needs_defsym(self) -> bool {
        self.stack_size.is_some() || self.initial_memory_bytes.is_some()
    }

    /// The `-C link-arg=--defsym=...` values the layout adds to the final
    /// rustc invocation. `__memory_size` takes effect only with an SDK linker
    /// script that `PROVIDE`s it; an older script fixes 64 KiB and ignores the
    /// request, while the artifact header carries the configured size either
    /// way.
    fn defsym_flags(self) -> Vec<String> {
        let mut flags = Vec::new();
        if let Some(size) = self.stack_size {
            flags.push(format!("link-arg=--defsym=__stack_size={size}"));
        }
        if let Some(size) = self.initial_memory_bytes {
            flags.push(format!("link-arg=--defsym=__memory_size={size}"));
        }
        flags
    }

    /// The stack reserve has to leave room for data below the address space.
    fn validate(self) -> Result<(), CliError> {
        if let Some(stack) = self.stack_size
            && stack as usize >= self.memory_bytes()
        {
            return Err(CliError::InvalidArgument {
                argument: "--stack-size".to_string(),
                message: format!(
                    "stack size {stack} must be below the {}-byte guest data memory",
                    self.memory_bytes()
                ),
            });
        }
        Ok(())
    }
}

/// The guest data address space `pkg_name`'s manifest declares, if any.
fn package_initial_memory_bytes(
    metadata: &CargoMetadata,
    pkg_name: &str,
) -> Result<Option<u32>, CliError> {
    let value = metadata
        .packages
        .iter()
        .find(|package| package.name == pkg_name)
        .and_then(|package| package.metadata.as_ref())
        .and_then(|table| table.groundhog.as_ref())
        .and_then(|groundhog| groundhog.initial_memory_bytes.as_ref());
    let Some(value) = value else {
        return Ok(None);
    };
    parse_initial_memory_bytes(value)
        .map(Some)
        .map_err(|error| CliError::PackageMetadataInvalid {
            package: pkg_name.to_string(),
            key: INITIAL_MEMORY_BYTES_KEY,
            message: error.to_string(),
        })
}

/// True when a requested stack reserve visibly did not happen: the lowered
/// image stages data below the requested stack top, which the stack-first
/// linker script never produces. Old SDK linker scripts ignore the
/// `__stack_size` defsym silently, so this is the only build-time signal.
fn stack_size_defsym_ignored(
    initial_memory_start: usize,
    initial_memory_len: usize,
    stack_size: u32,
) -> bool {
    initial_memory_len > 0 && initial_memory_start < stack_size as usize
}

/// Build the current project to an RV32 artifact by compiling an ELF and
/// lowering it through the in-process RV32 lowerer library.
///
/// `stack_size` overrides the guest's `__stack_size` linker symbol via a
/// `--defsym` on the final link (requires an SDK with the stack-first linker
/// script; older scripts ignore the symbol, which is reported as a warning).
/// The guest data address space comes from the package's
/// `[package.metadata.groundhog] initial_memory_bytes`, so every build of the
/// package, including the one under `groundhog test`, stamps the same size.
///
/// No chain bound is checked here: the build is offline and does not know
/// where the artifact will be deployed. It reports the code and memory sizes,
/// the lowerer warns past the protocol-default code limit, and the deploying
/// chain's RPC endpoint rejects an artifact its configuration does not admit.
#[allow(clippy::too_many_lines)]
pub fn build_rv32_project(
    package: Option<&str>,
    cargo_args: &[String],
    artifacts: &ArtifactOptions,
    explicit_profile: Option<&str>,
    stack_size: Option<u32>,
    bindings: &[String],
    print: &Print,
) -> Result<BuildResult, CliError> {
    ensure_no_env_rustflags()?;
    crate::require_cargo()?;

    let cwd = std::env::current_dir()?;
    let metadata = cargo_metadata_with_artifacts(artifacts)?;
    let pkg_name = resolve_package(&metadata, package, &cwd)?;
    let layout = GuestLayout {
        stack_size,
        initial_memory_bytes: package_initial_memory_bytes(&metadata, pkg_name)?,
    };
    layout.validate()?;
    check_contract_raw_spec_format(pkg_name, cargo_args, print)?;
    let package_root = metadata
        .packages
        .iter()
        .find(|p| p.name == pkg_name)
        .and_then(|p| p.manifest_path.parent())
        .map_or_else(|| metadata.workspace_root.clone(), Path::to_path_buf);

    print.phase("Checking build configuration...");
    check_rv32_build_config(&metadata.workspace_root, &package_root, print)?;
    let contract_toml = require_contract_toml(&metadata.workspace_root)?;
    artifacts::ensure_no_target_dir_passthrough_conflicts(
        artifacts,
        &metadata.workspace_root,
        cargo_args,
    )?;
    let profile = crate::config::resolve_profile(&metadata.workspace_root, explicit_profile)?;

    if let Some(bytes) = layout.initial_memory_bytes {
        print.status(format_args!(
            "Sizing guest data memory at {bytes} bytes ([package.metadata.groundhog] \
             {INITIAL_MEMORY_BYTES_KEY})"
        ));
    }
    print.phase(format_args!("Compiling {pkg_name} to RV32 ELF..."));

    // `--stack-size` and `initial_memory_bytes` switch to `cargo rustc`: its
    // trailing flags append to the final rustc invocation, which is the only
    // way to add a one-off `--defsym` on top of the project's own
    // .cargo/config.toml rustflags.
    // (`RUSTFLAGS` would replace that whole target rustflags list, losing the
    // linker script; `--config` joins rather than replaces, which is why the
    // contract.toml overlay below is safe but cannot carry rustflags.)
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&metadata.workspace_root)
        .arg("--config")
        .arg(&contract_toml)
        .arg(if layout.needs_defsym() {
            "rustc"
        } else {
            "build"
        })
        .arg("--release")
        .arg("--no-default-features")
        .arg("--features")
        .arg(RV32_ENTRYPOINTS_FEATURE)
        .arg("--target")
        .arg(RV32_TARGET_TRIPLE)
        .arg("-p")
        .arg(pkg_name)
        .arg("--bin")
        .arg(crate::scaffold::to_snake(pkg_name));

    artifacts.apply_to_command(&mut cmd);

    if print.is_compact_build() {
        cmd.arg("--message-format=json");
    }

    for arg in cargo_args {
        cmd.arg(arg);
    }
    if layout.needs_defsym() {
        cmd.arg("--");
        for flag in layout.defsym_flags() {
            cmd.arg("-C").arg(flag);
        }
    }

    let (cargo_stdout, cargo_stderr) = crate::run_cargo(&mut cmd, print)?;

    let elf = artifacts::cargo_rv32_elf_output_path(&metadata.target_directory, pkg_name);
    if !elf.try_exists()? {
        return Err(CliError::Rv32ElfNotFound(elf.display().to_string()));
    }

    let rv32 = artifacts::groundhog_rv32_output_path(&metadata.target_directory, pkg_name);
    let lower_metadata =
        artifacts::groundhog_rv32_metadata_output_path(&metadata.target_directory, pkg_name);
    if let Some(parent) = rv32.parent() {
        fs::create_dir_all(parent)?;
    }

    print.phase("Lowering RV32 ELF...");
    let elf_bytes = fs::read(&elf)?;
    let lowered = groundhog_rv32_lower::lower_with(
        &elf_bytes,
        &groundhog_rv32_lower::LowerOptions {
            memory_bytes: layout.memory_bytes(),
        },
    )?;
    for warning in &lowered.warnings {
        print.warn(warning);
    }
    if let Some(size) = stack_size
        && stack_size_defsym_ignored(
            lowered.initial_memory_start,
            lowered.initial_memory.len(),
            size,
        )
    {
        print.warn(format_args!(
            "--stack-size {size} appears to have had no effect: data is staged at \
             {:#x}, below the requested stack top. The project's SDK linker script \
             likely predates the stack-first layout (`__stack_size` support).",
            lowered.initial_memory_start
        ));
    }
    let lower_metadata_doc =
        groundhog_rv32_lower::render_metadata(&groundhog_rv32_lower::MetadataInput {
            elf: &elf_bytes,
            artifact: &lowered.artifact,
            code: &lowered.code,
            initial_memory: &lowered.initial_memory,
            initial_memory_start: lowered.initial_memory_start,
            memory_bytes: lowered.memory_bytes,
            rustc_version: rustc_version(),
            target_triple: Some(RV32_TARGET_TRIPLE.to_string()),
            build_flags: Some(rv32_build_flags(pkg_name, cargo_args, layout)),
        });
    fs::write(&rv32, &lowered.artifact)?;
    if let Err(error) = fs::write(&lower_metadata, lower_metadata_doc) {
        let _ = fs::remove_file(&rv32);
        return Err(error.into());
    }

    print.phase("Checking for contract spec...");
    let finalized = crate::contract_spec::finalize_rv32_spec(&elf, &rv32, print)?;
    let spec_json = finalized.as_ref().map(|f| f.json_path.clone());
    let spec_xdr = finalized.as_ref().map(|f| f.xdr_path.clone());
    let source_name = rv32
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");
    let mut warnings = Vec::new();
    let bindings_ts = if bindings.iter().any(|binding| binding == "ts") {
        if let Some(ref finalized) = finalized {
            print.phase("Generating TypeScript bindings...");
            let ts_path = rv32.with_extension("ts");
            let ts = crate::codegen::generate_ts_bindings(&finalized.spec, source_name)?;
            fs::write(&ts_path, ts.as_bytes())?;
            print.status(format_args!("Wrote {}", ts_path.display()));
            Some(ts_path)
        } else {
            print.warn("no contract spec found — skipping TypeScript bindings");
            None
        }
    } else {
        if let Some(ref finalized) = finalized
            && let Some(warning) = ts_bindings_warning(&finalized.spec, source_name)
        {
            print.warn(&warning.message);
            warnings.push(warning);
        }
        None
    };

    let size = fs::metadata(&rv32)?.len();
    if !print.in_task() {
        print.status(format_args!(
            "Built {} ({size} bytes; code {} bytes, guest data memory {} bytes)",
            rv32.display(),
            lowered.code.len(),
            lowered.memory_bytes
        ));
    }

    Ok(BuildResult {
        project_name: pkg_name.to_string(),
        project_root: metadata.workspace_root,
        package_root,
        target_directory: metadata.target_directory,
        elf,
        rv32,
        metadata: lower_metadata,
        spec_json,
        spec_xdr,
        bindings_ts,
        size_bytes: size,
        code_bytes: lowered.code.len(),
        memory_bytes: lowered.memory_bytes,
        target: ArtifactKind::Rv32,
        profile,
        cargo_stdout,
        cargo_stderr,
        warnings,
    })
}

/// The warning for a spec `groundhog codegen ts` would refuse, so the author
/// hears at build time rather than when someone generates bindings.
fn ts_bindings_warning(spec: &ContractSpec, source_name: &str) -> Option<Finding> {
    let refusal = crate::codegen::generate_ts_bindings(spec, source_name).err()?;
    Some(Finding {
        level: FindingLevel::Warn,
        code: refusal.code(),
        message: refusal.to_string(),
        hint: None,
    })
}

/// The feature the rv32 build enables so the `#[contract]` macro emits guest
/// entrypoints and the bin wrapper target becomes buildable. The macro's cfg
/// matches this literal name, so it is not configurable per project.
const RV32_ENTRYPOINTS_FEATURE: &str = "entrypoints";

/// The cargo invocation recorded in the `.rv32.json` sidecar.
///
/// The `--config` overlay is recorded as its workspace-relative path (the
/// build runs with the workspace root as its working directory) so the
/// recorded command reproduces the artifact without embedding the absolute
/// path of the machine that built it. It cannot be omitted: the overlay
/// carries `build-std`, which changes codegen.
fn rv32_build_flags(pkg_name: &str, cargo_args: &[String], layout: GuestLayout) -> String {
    let mut flags = vec![
        "cargo".to_string(),
        "--config".to_string(),
        CONTRACT_TOML_RELATIVE_PATH.to_string(),
        if layout.needs_defsym() {
            "rustc"
        } else {
            "build"
        }
        .to_string(),
        "--release".to_string(),
        "--no-default-features".to_string(),
        "--features".to_string(),
        RV32_ENTRYPOINTS_FEATURE.to_string(),
        "--target".to_string(),
        RV32_TARGET_TRIPLE.to_string(),
        "-p".to_string(),
        pkg_name.to_string(),
        "--bin".to_string(),
        crate::scaffold::to_snake(pkg_name),
    ];
    flags.extend(cargo_args.iter().cloned());
    if layout.needs_defsym() {
        flags.push("--".to_string());
        for flag in layout.defsym_flags() {
            flags.push("-C".to_string());
            flags.push(flag);
        }
    }
    flags.join(" ")
}

fn rustc_version() -> Option<String> {
    let output = Command::new("rustc").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, serde::Deserialize)]
pub struct CargoMetadata {
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub packages: Vec<MetadataPackage>,
}

#[derive(Debug, serde::Deserialize)]
pub struct MetadataPackage {
    pub name: String,
    /// Version cargo resolved for this package, with any workspace
    /// inheritance already applied.
    pub version: String,
    pub manifest_path: PathBuf,
    /// The package's `[package.metadata]` table; `None` when it has none.
    #[serde(default)]
    pub metadata: Option<PackageMetadata>,
    /// The package's build targets: its lib, bins, tests, and so on.
    #[serde(default)]
    pub targets: Vec<MetadataTarget>,
}

impl MetadataPackage {
    /// True when the package declares a test target with this name, whether
    /// auto-discovered under `tests/` or listed as `[[test]]`.
    pub fn has_test_target(&self, name: &str) -> bool {
        self.targets
            .iter()
            .any(|target| target.name == name && target.kind.iter().any(|kind| kind == "test"))
    }
}

/// One build target of a package, as far as the CLI reads it.
#[derive(Debug, serde::Deserialize)]
pub struct MetadataTarget {
    pub name: String,
    /// The target kinds cargo reports, such as `lib`, `bin`, or `test`.
    pub kind: Vec<String>,
}

/// A package's `[package.metadata]` table, as far as the CLI reads it.
#[derive(Debug, serde::Deserialize)]
pub struct PackageMetadata {
    /// The `[package.metadata.groundhog]` table.
    pub groundhog: Option<GroundhogPackageMetadata>,
}

/// The `[package.metadata.groundhog]` keys the CLI reads.
#[derive(Debug, serde::Deserialize)]
pub struct GroundhogPackageMetadata {
    /// Declared by `groundhog-contract-spec`: the raw spec wire format it
    /// emits.
    pub raw_spec_format: Option<u32>,
    /// Declared by a contract: the guest data address space it wants
    /// ([`INITIAL_MEMORY_BYTES_KEY`]).
    pub initial_memory_bytes: Option<ByteSizeValue>,
}

/// Resolve which package to build.
///
/// When `package` is `Some`, validates the name exists in the
/// metadata.  When `None`, picks the package whose manifest
/// directory is the longest prefix of `cwd`.
pub fn resolve_package<'a>(
    metadata: &'a CargoMetadata,
    package: Option<&'a str>,
    cwd: &Path,
) -> Result<&'a str, CliError> {
    if let Some(name) = package {
        if metadata.packages.iter().any(|p| p.name == name) {
            return Ok(name);
        }
        return Err(CliError::PackageNotFound(name.to_string()));
    }

    metadata
        .packages
        .iter()
        .filter(|p| {
            p.manifest_path
                .parent()
                .is_some_and(|dir| cwd.starts_with(dir))
        })
        .max_by_key(|p| p.manifest_path.parent().map_or(0, |d| d.as_os_str().len()))
        .map(|p| p.name.as_str())
        .ok_or(CliError::PackageRequired)
}

pub fn cargo_metadata_with_artifacts(
    artifacts: &ArtifactOptions,
) -> Result<CargoMetadata, CliError> {
    let mut command = Command::new("cargo");
    command
        .arg("metadata")
        .arg("--color=never")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps");
    artifacts.apply_to_command(&mut command);

    let output = command
        .output()
        .map_err(|e| CliError::CargoMetadataFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::CargoMetadataFailed(stderr.trim().to_string()));
    }

    serde_json::from_slice(&output.stdout).map_err(|e| CliError::CargoMetadataFailed(e.to_string()))
}
/// The SDK crate whose manifest declares the raw spec wire format a contract
/// emits. It owns the fragment types, so the format version travels with it
/// and no version table has to be maintained here.
const CONTRACT_SPEC_PACKAGE: &str = "groundhog-contract-spec";

/// `cargo metadata` output read for the resolved dependency graph.
///
/// Separate from [`CargoMetadata`], which is produced with `--no-deps` and so
/// reports only workspace members.
#[derive(Debug, serde::Deserialize)]
pub struct DependencyMetadata {
    packages: Vec<DependencyPackage>,
    /// Absent when cargo was asked not to resolve (`--no-deps`).
    resolve: Option<DependencyResolve>,
}

#[derive(Debug, serde::Deserialize)]
struct DependencyResolve {
    nodes: Vec<DependencyNode>,
}

#[derive(Debug, serde::Deserialize)]
struct DependencyNode {
    id: String,
    dependencies: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct DependencyPackage {
    id: String,
    name: String,
    /// `null` for a package with no `[package.metadata]` table.
    metadata: Option<PackageMetadata>,
}

impl DependencyPackage {
    /// The raw spec format this package declares.
    ///
    /// A `groundhog-contract-spec` predating the manifest key is
    /// pre-versioning, which is format 0 — genuinely unsupported, so absence
    /// is not treated as "unknown, allow it".
    fn raw_spec_format(&self) -> u32 {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.groundhog.as_ref())
            .and_then(|groundhog| groundhog.raw_spec_format)
            .unwrap_or(0)
    }
}

/// Run a `cargo metadata` call, relaying cargo's progress as it arrives.
///
/// `Command::output` withholds that progress until the call returns, which is
/// what turned a registry outage into a build that printed nothing while
/// cargo waited — retrying an unreachable index, or blocking on the
/// package-cache lock, which it holds with no timeout at all. Relaying it
/// keeps a slow or stalled registry legible while it happens, so the wait is
/// the user's to end rather than something the build hides.
///
/// Deliberately not bounded by a deadline: cargo already times out its own
/// requests, and a stall is indistinguishable from a slow link by the clock
/// alone — a resolve that is merely crawling over bad wifi must not be
/// failed out from under a build that would have finished.
///
/// Cargo's stdout is staged through a file rather than a pipe, because the
/// metadata document runs to megabytes and a pipe nobody is draining would
/// stop cargo mid-write.
fn run_cargo_metadata(command: &mut Command, print: &Print) -> Result<Vec<u8>, CliError> {
    fn failed(error: impl std::fmt::Display) -> CliError {
        CliError::CargoMetadataFailed(error.to_string())
    }

    let staged_stdout = tempfile::NamedTempFile::new().map_err(failed)?;
    let relay = !print.is_json() && !print.is_quiet();
    if relay || print.is_non_interactive() {
        crate::keep_cargo_color(command, print);
    }
    command
        .stdin(Stdio::null())
        .stdout(staged_stdout.reopen().map_err(failed)?)
        .stderr(Stdio::piped());
    print.trace_command(command);

    let mut child = command.spawn().map_err(failed)?;
    let stderr_pipe = child
        .stderr
        .take()
        .expect("bug: child stderr is piped above");
    let captured = match crate::capture_cargo_stderr(stderr_pipe, relay, print) {
        Ok(captured) => captured,
        Err(error) => {
            crate::stop_cargo(&mut child);
            return Err(failed(error));
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            crate::stop_cargo(&mut child);
            return Err(failed(error));
        }
    };
    if !status.success() {
        return Err(CliError::CargoMetadataFailed(
            String::from_utf8_lossy(&captured).trim().to_string(),
        ));
    }

    let mut reader = fs::File::open(staged_stdout.path()).map_err(failed)?;
    let mut metadata = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut metadata).map_err(failed)?;
    Ok(metadata)
}

/// Run `cargo metadata` over the resolved dependency graph.
///
/// Deliberately a separate call from [`cargo_metadata_with_artifacts`], which
/// passes `--no-deps`: resolving the full graph is slower and can hit the
/// network on a cold checkout, and no other caller needs dependencies.
///
/// This does not forward the user's whole `cargo_args`, so it takes
/// [`ManifestFlags`] explicitly: resolving the graph is exactly what those
/// flags restrict, and dropping them here would let a `--locked` build's
/// preflight rewrite the lockfile or an `--offline` build reach the network.
fn cargo_metadata_with_deps(
    flags: ManifestFlags,
    print: &Print,
) -> Result<DependencyMetadata, CliError> {
    let mut command = Command::new("cargo");
    command.arg("metadata").arg("--format-version").arg("1");
    flags.apply_to_command(&mut command);

    let stdout = run_cargo_metadata(&mut command, print)?;
    serde_json::from_slice(&stdout).map_err(|e| CliError::CargoMetadataFailed(e.to_string()))
}

/// Refuse a contract whose SDK writes a raw spec format this CLI cannot read.
///
/// Scoped to what `pkg_name` actually resolves, since `cargo metadata` reports
/// the whole workspace graph but the build is scoped with `-p`: an unrelated
/// member pinning an SDK this build cannot read must not refuse a package that
/// never depends on it.
///
/// Every copy reachable from the selected package is checked. Two SDK versions
/// under one contract is already a broken build, so an unreadable copy refuses
/// rather than being resolved arbitrarily in favor of a readable one.
///
/// Returns `Ok(())` when the graph cannot be scoped — an unresolved metadata
/// document, or a package cargo did not report. Refusing on a copy that may
/// belong to something else is worse than leaving the skew to the
/// post-compile check.
fn check_dependency_raw_spec_format(
    metadata: &DependencyMetadata,
    pkg_name: &str,
) -> Result<(), CliError> {
    let Some(resolve) = metadata.resolve.as_ref() else {
        return Ok(());
    };
    let Some(root) = metadata
        .packages
        .iter()
        .find(|package| package.name == pkg_name)
    else {
        return Ok(());
    };

    let nodes: std::collections::HashMap<&str, &DependencyNode> = resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut seen = std::collections::HashSet::from([root.id.as_str()]);
    let mut queue = vec![root.id.as_str()];

    while let Some(id) = queue.pop() {
        let Some(node) = nodes.get(id) else { continue };
        for dependency in &node.dependencies {
            // `insert` returns false for an id already walked, which is what
            // terminates the walk on a dependency cycle.
            if seen.insert(dependency.as_str()) {
                queue.push(dependency.as_str());
            }
        }
    }

    for package in &metadata.packages {
        if package.name != CONTRACT_SPEC_PACKAGE || !seen.contains(package.id.as_str()) {
            continue;
        }
        groundhog_spec_tool::check_raw_spec_format(package.raw_spec_format())
            .map_err(CliError::RawSpecFormat)?;
    }
    Ok(())
}

/// Verify the contract's raw spec format before compiling it.
///
/// The raw spec section is written by the SDK compiled into the contract and
/// read by the `groundhog-spec-tool` linked into this CLI, and the two are
/// released independently. Reading the format out of the dependency graph
/// costs one `cargo metadata` call, so skew is reported in seconds instead of
/// as a fragment parse failure after a full contract compile.
///
/// A project with no `groundhog-contract-spec` in its graph is not a
/// Groundhog contract and is left alone. An unavailable dependency graph
/// warns rather than refuses, so an offline or vendored build still
/// completes: only a format this build genuinely cannot read is a hard error.
///
/// This is the first thing a build does that reads the registry, so it is
/// also where a registry problem first shows up. Cargo says what it is
/// waiting on, and this relays it rather than deciding on the user's behalf
/// how long that wait is allowed to be.
fn check_contract_raw_spec_format(
    pkg_name: &str,
    cargo_args: &[String],
    print: &Print,
) -> Result<(), CliError> {
    print.phase("Resolving the dependency graph...");
    match cargo_metadata_with_deps(ManifestFlags::from_cargo_args(cargo_args), print) {
        Ok(metadata) => check_dependency_raw_spec_format(&metadata, pkg_name),
        Err(error) => {
            print.warn(format_args!(
                "could not check which raw contract-spec section this \
                 contract emits ({error}); this CLI reads \
                 `{}` — the embedded spec section format, not a crate \
                 version — so a mismatch will now surface as a spec parse \
                 failure after the build instead",
                groundhog_spec_tool::RAW_SECTION_NAME
            ));
            Ok(())
        }
    }
}

/// The linker script the Groundhog SDK supplies for rv32 builds.
pub(crate) const RV32_LINKER_SCRIPT: &str = "groundhog-rv32.ld";

/// What cargo's effective config says about the rv32 build.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rv32ConfigCheck {
    /// The Groundhog linker script is in the effective rustflags.
    GroundhogScript,
    /// A different `-T` linker script is configured.
    CustomScript(String),
    /// Rustflags resolve (or are unset) without any `-T` linker script.
    NoScript,
    /// Cargo rejected the probe: the toolchain resolved at the workspace
    /// root is not nightly, which `cargo config` requires.
    NotNightly,
    /// The probe failed some other way; carries a short reason.
    Unverifiable(String),
}

/// Verify the rv32 build config cargo will actually use before compiling.
///
/// Cargo discovers config from its invocation directory upward plus
/// `CARGO_HOME`, and the build runs cargo from the workspace root — so a
/// project wrapped into a parent workspace after `groundhog new` leaves the
/// generated config behind in the member directory where cargo never reads
/// it. Linking without a `-T` script cannot produce a valid artifact, so
/// that refuses with the copy-the-config hint; every other imperfect probe
/// outcome warns with its diagnosis and lets the build proceed.
fn check_rv32_build_config(
    workspace_root: &Path,
    package_root: &Path,
    print: &Print,
) -> Result<(), CliError> {
    match probe_rv32_config(workspace_root) {
        Rv32ConfigCheck::GroundhogScript => Ok(()),
        Rv32ConfigCheck::CustomScript(script) => {
            print.warn(format_args!(
                "the effective rv32 rustflags select linker script `-T{script}` instead of \
                 `-T{RV32_LINKER_SCRIPT}`; the build proceeds, but the script must preserve \
                 the Groundhog memory layout for the artifact to validate"
            ));
            Ok(())
        }
        Rv32ConfigCheck::NoScript => Err(CliError::Rv32LinkerScriptNotConfigured {
            workspace_root: workspace_root.display().to_string(),
            member_config: member_leftover_cargo_config(workspace_root, package_root)
                .map(|path| path.display().to_string()),
        }),
        Rv32ConfigCheck::NotNightly => {
            print.warn(format_args!(
                "could not verify the rv32 build config: cargo at the workspace root {} is \
                 not a nightly toolchain, which Groundhog builds require; if this project \
                 was wrapped into a workspace, copy `rust-toolchain.toml` to the workspace \
                 root alongside `.cargo/config.toml`",
                workspace_root.display()
            ));
            Ok(())
        }
        Rv32ConfigCheck::Unverifiable(reason) => {
            print.warn(format_args!(
                "could not verify the rv32 build config (`cargo config get` failed: {reason})"
            ));
            Ok(())
        }
    }
}

/// Ask cargo for the effective `target.riscv32im-unknown-none-elf.rustflags`
/// as seen from the workspace root, where the build runs — cargo applies its
/// own config discovery (invocation directory upward, `CARGO_HOME`, and
/// environment overrides). A `RUSTFLAGS` environment override is invisible
/// to `cargo config get`, which is why `ensure_no_env_rustflags` guards it
/// separately.
fn probe_rv32_config(workspace_root: &Path) -> Rv32ConfigCheck {
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "config",
            "get",
            &format!("target.{RV32_TARGET_TRIPLE}.rustflags"),
            "--format",
            "json-value",
            "-Zunstable-options",
        ])
        .output();
    match output {
        Ok(output) => classify_rv32_config_probe(
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(error) => Rv32ConfigCheck::Unverifiable(error.to_string()),
    }
}

fn classify_rv32_config_probe(success: bool, stdout: &str, stderr: &str) -> Rv32ConfigCheck {
    if !success {
        return if stderr.contains("is not set") {
            Rv32ConfigCheck::NoScript
        } else if stderr.contains("nightly") {
            Rv32ConfigCheck::NotNightly
        } else {
            Rv32ConfigCheck::Unverifiable(
                stderr
                    .lines()
                    .next()
                    .unwrap_or("no error output")
                    .to_string(),
            )
        };
    }
    let tokens: Vec<String> = match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(serde_json::Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        Ok(serde_json::Value::String(flags)) => {
            flags.split_whitespace().map(str::to_string).collect()
        }
        _ => {
            return Rv32ConfigCheck::Unverifiable(
                "unexpected `cargo config get` output".to_string(),
            );
        }
    };
    match rv32_linker_script(&tokens) {
        Some(script) if script == RV32_LINKER_SCRIPT => Rv32ConfigCheck::GroundhogScript,
        Some(script) => Rv32ConfigCheck::CustomScript(script),
        None => Rv32ConfigCheck::NoScript,
    }
}

/// The linker script selected by a `-T` inside `link-arg=`/`link-args=`
/// rustflags tokens, if any.
fn rv32_linker_script(tokens: &[String]) -> Option<String> {
    tokens.iter().find_map(|token| {
        let (_, value) = token
            .split_once("link-arg=")
            .or_else(|| token.split_once("link-args="))?;
        value
            .split_whitespace()
            .find_map(|arg| arg.strip_prefix("-T"))
            .map(str::to_string)
    })
}

/// The generated cargo config a workspace member kept after being wrapped in
/// a parent workspace, honoring cargo's legacy extension-less `config` name.
fn member_leftover_cargo_config(workspace_root: &Path, package_root: &Path) -> Option<PathBuf> {
    if package_root == workspace_root {
        return None;
    }
    [".cargo/config.toml", ".cargo/config"]
        .into_iter()
        .map(|name| package_root.join(name))
        .find(|path| path.is_file())
}

/// The CLI-only cargo config overlay, relative to the workspace root.
///
/// It carries `[unstable] build-std`, which both contract targets need and
/// which cannot live in `.cargo/config.toml` — `[unstable]` is global rather
/// than per-target, so it would also apply to host `cargo test` runs.
const CONTRACT_TOML_RELATIVE_PATH: &str = ".cargo/contract.toml";

/// Check that `.cargo/contract.toml` exists at the workspace root.
fn require_contract_toml(workspace_root: &Path) -> Result<PathBuf, CliError> {
    let path = workspace_root.join(CONTRACT_TOML_RELATIVE_PATH);
    if path.try_exists()? {
        Ok(path)
    } else {
        Err(CliError::ContractTomlNotFound(
            workspace_root.display().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ByteSizeValue, CliError, ContractSpec, FindingLevel, GuestLayout, Path, Rv32ConfigCheck,
        classify_rv32_config_probe, member_leftover_cargo_config, parse_initial_memory_bytes,
        parse_stack_size, rv32_linker_script, stack_size_defsym_ignored, ts_bindings_warning,
    };
    use crate::codegen::generate_ts_bindings;
    use crate::contract_spec::load_contract_spec_from_xdr;
    use groundhog_contract_spec::{ContractStructDef, ContractTypeDef};
    use tempfile::TempDir;

    fn tokens(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|token| (*token).to_string()).collect()
    }

    #[test]
    fn probe_accepts_groundhog_script_in_array_flags() {
        assert_eq!(
            classify_rv32_config_probe(
                true,
                r#"["-C","target-feature=+unaligned-scalar-mem","-C","link-arg=-Tgroundhog-rv32.ld","-C","link-arg=-static"]"#,
                ""
            ),
            Rv32ConfigCheck::GroundhogScript
        );
    }

    #[test]
    fn probe_accepts_groundhog_script_in_string_flags() {
        assert_eq!(
            classify_rv32_config_probe(true, r#""-C link-arg=-Tgroundhog-rv32.ld""#, ""),
            Rv32ConfigCheck::GroundhogScript
        );
    }

    #[test]
    fn probe_reports_custom_linker_script() {
        assert_eq!(
            classify_rv32_config_probe(true, r#"["-C","link-arg=-Tcustom.ld"]"#, ""),
            Rv32ConfigCheck::CustomScript("custom.ld".into())
        );
    }

    #[test]
    fn probe_flags_without_linker_script_mean_no_script() {
        assert_eq!(
            classify_rv32_config_probe(true, r#"["-C","opt-level=3"]"#, ""),
            Rv32ConfigCheck::NoScript
        );
    }

    #[test]
    fn probe_unset_rustflags_mean_no_script() {
        assert_eq!(
            classify_rv32_config_probe(
                false,
                "",
                "error: config value `target.riscv32im-unknown-none-elf.rustflags` is not set\n"
            ),
            Rv32ConfigCheck::NoScript
        );
    }

    #[test]
    fn probe_detects_non_nightly_toolchain() {
        assert_eq!(
            classify_rv32_config_probe(
                false,
                "",
                "error: the `-Z` flag is only accepted on the nightly channel of Cargo, but \
                 this is the `stable` channel\n"
            ),
            Rv32ConfigCheck::NotNightly
        );
    }

    #[test]
    fn probe_reports_other_failures_as_unverifiable() {
        let checked = classify_rv32_config_probe(false, "", "error: something exploded\nmore\n");
        assert_eq!(
            checked,
            Rv32ConfigCheck::Unverifiable("error: something exploded".into())
        );
        // Successful probe with output the classifier cannot parse is equally
        // inconclusive.
        assert!(matches!(
            classify_rv32_config_probe(true, "not json", ""),
            Rv32ConfigCheck::Unverifiable(_)
        ));
    }

    #[test]
    fn linker_script_extraction_handles_flag_spellings() {
        assert_eq!(
            rv32_linker_script(&tokens(&["-Clink-arg=-Tfoo.ld"])),
            Some("foo.ld".into())
        );
        assert_eq!(
            rv32_linker_script(&tokens(&["link-args=-zstack-size=30000 -Tbar.ld"])),
            Some("bar.ld".into())
        );
        assert_eq!(rv32_linker_script(&tokens(&["-C", "opt-level=3"])), None);
    }

    #[test]
    fn member_leftover_config_reported_only_for_distinct_member_dir() {
        let tmp = TempDir::new().unwrap();
        let member = tmp.path().join("member");
        std::fs::create_dir_all(member.join(".cargo")).unwrap();
        std::fs::write(member.join(".cargo/config.toml"), "").unwrap();

        assert_eq!(
            member_leftover_cargo_config(tmp.path(), &member),
            Some(member.join(".cargo/config.toml"))
        );
        // A single-package project is its own workspace root; nothing to name.
        assert_eq!(member_leftover_cargo_config(&member, &member), None);
    }

    #[test]
    fn parse_stack_size_accepts_decimal_hex_and_unit_suffixes() {
        assert_eq!(parse_stack_size("32768"), Ok(32768));
        assert_eq!(parse_stack_size("0x8000"), Ok(32768));
        assert_eq!(parse_stack_size("0x6000"), Ok(24576));
        assert_eq!(parse_stack_size("16"), Ok(16));
        assert_eq!(parse_stack_size("65520"), Ok(65520)); // last multiple of 16 below 64K
        assert_eq!(parse_stack_size("64K"), Ok(65536));
        assert_eq!(parse_stack_size("32K"), Ok(32768));
        assert_eq!(parse_stack_size("24k"), Ok(24576));
        assert_eq!(parse_stack_size("32KiB"), Ok(32768));
    }

    #[test]
    fn parse_stack_size_rejects_invalid_values() {
        for bad in [
            "0", "0x0", "24", "0x6008", "", "abc", "0x", "-16", "2.5K", "5G",
        ] {
            assert!(
                parse_stack_size(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn stack_size_is_bounded_by_the_guest_memory_not_the_parser() {
        // A stack the size of the whole default address space parses; whether
        // it fits is decided against the memory the package declares.
        for at_or_over in ["65536", "0x10000", "64K", "1M"] {
            let stack = parse_stack_size(at_or_over).expect("parses");
            let under_default = GuestLayout {
                stack_size: Some(stack),
                initial_memory_bytes: None,
            };
            assert!(
                matches!(
                    under_default.validate(),
                    Err(CliError::InvalidArgument { .. })
                ),
                "{at_or_over} vs 64K"
            );
            let under_raised = GuestLayout {
                stack_size: Some(stack),
                initial_memory_bytes: Some(2 * 1024 * 1024),
            };
            assert!(under_raised.validate().is_ok(), "{at_or_over} vs 2M");
        }
        let last_fit = GuestLayout {
            stack_size: Some(0xFFF0),
            initial_memory_bytes: None,
        };
        assert!(last_fit.validate().is_ok());
    }

    #[test]
    fn parse_initial_memory_bytes_accepts_integers_and_byte_size_strings() {
        assert_eq!(
            parse_initial_memory_bytes(&ByteSizeValue::Integer(131_072)),
            Ok(131_072)
        );
        assert_eq!(
            parse_initial_memory_bytes(&ByteSizeValue::Text("128K".into())),
            Ok(131_072)
        );
        assert_eq!(
            parse_initial_memory_bytes(&ByteSizeValue::Text("0x20000".into())),
            Ok(131_072)
        );
        assert_eq!(
            parse_initial_memory_bytes(&ByteSizeValue::Text("1M".into())),
            Ok(1_048_576)
        );
        for bad in [
            ByteSizeValue::Integer(0),
            ByteSizeValue::Integer(-16),
            ByteSizeValue::Integer(1 << 32),
            ByteSizeValue::Text("0".into()),
            ByteSizeValue::Text("5G".into()),
            ByteSizeValue::Text("abc".into()),
        ] {
            assert!(parse_initial_memory_bytes(&bad).is_err(), "input {bad:?}");
        }
    }

    #[test]
    fn guest_layout_defaults_to_the_protocol_memory_and_no_defsym() {
        let layout = GuestLayout::default();
        assert_eq!(
            layout.memory_bytes(),
            groundhog_rv32_artifact::DEFAULT_MEMORY_BYTES
        );
        assert!(!layout.needs_defsym());
        assert!(layout.defsym_flags().is_empty());
        assert!(layout.validate().is_ok());
    }

    #[test]
    fn guest_layout_passes_both_symbols_on_the_same_link() {
        let layout = GuestLayout {
            stack_size: Some(0x6000),
            initial_memory_bytes: Some(0x20000),
        };
        assert_eq!(layout.memory_bytes(), 0x20000);
        assert!(layout.needs_defsym());
        assert_eq!(
            layout.defsym_flags(),
            vec![
                "link-arg=--defsym=__stack_size=24576".to_string(),
                "link-arg=--defsym=__memory_size=131072".to_string(),
            ]
        );
        assert!(layout.validate().is_ok());

        // Memory alone still needs the `cargo rustc` path.
        let memory_only = GuestLayout {
            stack_size: None,
            initial_memory_bytes: Some(0x20000),
        };
        assert!(memory_only.needs_defsym());
        assert_eq!(
            memory_only.defsym_flags(),
            vec!["link-arg=--defsym=__memory_size=131072".to_string()]
        );
    }

    #[test]
    fn stack_size_defsym_ignored_detects_pre_stack_first_layouts() {
        // Data staged below the requested stack top: the linker script did not
        // honor __stack_size, so the reserve is not actually there.
        assert!(stack_size_defsym_ignored(0x8, 4, 0x6000));
        // Data staged at (or above) the stack top: layout honored the reserve.
        assert!(!stack_size_defsym_ignored(0x6000, 4, 0x6000));
        assert!(!stack_size_defsym_ignored(0x7000, 4, 0x6000));
        // No stored data at all: nothing to infer, stay quiet.
        assert!(!stack_size_defsym_ignored(0, 0, 0x6000));
    }

    fn codegen_fixture_spec(name: &str) -> ContractSpec {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/codegen")
            .join(format!("{name}.spec.xdr"));
        load_contract_spec_from_xdr(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_spec_the_codegen_accepts_raises_no_bindings_warning() {
        let spec = codegen_fixture_spec("typed_contract");

        assert!(ts_bindings_warning(&spec, "typed_contract.rv32a").is_none());
    }

    #[test]
    fn a_spec_the_codegen_refuses_raises_its_refusal_as_a_warning() {
        // A struct on an event's path, where the bindings cannot declare both.
        let mut spec = codegen_fixture_spec("typed_contract");
        let name = spec.events.0[0].name.clone();
        spec.type_defs.0.push(ContractTypeDef::TypeDefStruct {
            struct_def: ContractStructDef {
                name,
                fields: vec![].try_into().unwrap(),
            },
        });

        let warning = ts_bindings_warning(&spec, "typed_contract.rv32a")
            .expect("the codegen refuses a struct and an event on one path");

        let refusal = generate_ts_bindings(&spec, "typed_contract.rv32a").unwrap_err();
        assert_eq!(warning.level, FindingLevel::Warn);
        assert_eq!(warning.code, "ts_bindings_failed");
        assert_eq!(warning.message, refusal.to_string());
        assert!(
            warning.message.contains("collides between struct"),
            "{}",
            warning.message
        );
    }
}

#[cfg(test)]
mod raw_spec_format_preflight_tests {
    use groundhog_spec_tool::RAW_SPEC_FORMAT;

    const SELECTED: &str = "demo-contract";
    const SELECTED_ID: &str = "path+file:///ws/demo#demo-contract@0.1.0";
    const UNRELATED_ID: &str = "path+file:///ws/other#other-member@0.1.0";

    /// What a workspace member reaches, spec-wise.
    #[derive(Clone, Copy)]
    enum SpecDep {
        /// No `groundhog-contract-spec` in this member's subtree.
        Absent,
        /// A copy declaring this raw spec format.
        Format(u32),
        /// A copy predating `[package.metadata.groundhog] raw_spec_format`.
        PreVersioning,
    }

    /// `cargo metadata` for a two-member workspace: the package being built
    /// depends on a `groundhog-contract-spec` declaring `selected`, and an
    /// unrelated member depends on a second copy declaring `unrelated`.
    ///
    /// `SpecDep::Absent` means the member reaches no
    /// `groundhog-contract-spec` at all.
    fn workspace_json(selected: SpecDep, unrelated: SpecDep) -> String {
        const SELECTED_SPEC_ID: &str = "registry+groundhog#groundhog-contract-spec@0.19.0";
        const UNRELATED_SPEC_ID: &str = "registry+groundhog#groundhog-contract-spec@0.18.2";

        let mut packages = vec![
            format!(r#"{{"id":"{SELECTED_ID}","name":"{SELECTED}","metadata":null}}"#),
            format!(r#"{{"id":"{UNRELATED_ID}","name":"other-member","metadata":null}}"#),
        ];
        let mut node = |member_id: &str, spec_id: &str, dep: SpecDep| {
            let table = match dep {
                SpecDep::Absent => {
                    return format!(r#"{{"id":"{member_id}","dependencies":[]}}"#);
                }
                SpecDep::Format(format) => {
                    format!(r#"{{"groundhog":{{"raw_spec_format":{format}}}}}"#)
                }
                SpecDep::PreVersioning => "null".to_string(),
            };
            packages.push(format!(
                r#"{{"id":"{spec_id}","name":"groundhog-contract-spec","metadata":{table}}}"#
            ));
            format!(r#"{{"id":"{member_id}","dependencies":["{spec_id}"]}}"#)
        };
        let nodes = [
            node(SELECTED_ID, SELECTED_SPEC_ID, selected),
            node(UNRELATED_ID, UNRELATED_SPEC_ID, unrelated),
        ];

        format!(
            r#"{{"packages":[{}],"resolve":{{"nodes":[{}]}}}}"#,
            packages.join(","),
            nodes.join(",")
        )
    }

    fn check(json: &str) -> Result<(), crate::CliError> {
        let metadata = serde_json::from_str(json).expect("bug: fixture should parse");
        super::check_dependency_raw_spec_format(&metadata, SELECTED)
    }

    #[test]
    fn accepts_the_format_this_build_reads() {
        let json = workspace_json(SpecDep::Format(RAW_SPEC_FORMAT), SpecDep::Absent);

        assert!(check(&json).is_ok());
    }

    #[test]
    fn refuses_a_newer_format() {
        let newer = RAW_SPEC_FORMAT + 1;
        let json = workspace_json(SpecDep::Format(newer), SpecDep::Absent);

        let message = check(&json)
            .expect_err("a newer SDK format must refuse the build")
            .to_string();

        assert!(
            message.contains("newer Groundhog SDK") && message.contains(&newer.to_string()),
            "expected a version-skew diagnosis naming {newer}, got: {message}"
        );
    }

    #[test]
    fn refuses_an_older_format() {
        let json = workspace_json(SpecDep::Format(RAW_SPEC_FORMAT - 1), SpecDep::Absent);

        let message = check(&json)
            .expect_err("an older SDK format must refuse the build")
            .to_string();

        assert!(
            message.contains("older Groundhog SDK"),
            "expected a version-skew diagnosis, got: {message}"
        );
    }

    #[test]
    fn treats_a_contract_spec_without_the_key_as_pre_versioning() {
        // An SDK predating `[package.metadata.groundhog] raw_spec_format` is
        // format 0, which is unsupported in both directions.
        let message = check(&workspace_json(SpecDep::PreVersioning, SpecDep::Absent))
            .expect_err("a pre-versioning SDK must refuse the build")
            .to_string();

        assert!(
            message.contains("older Groundhog SDK"),
            "expected a version-skew diagnosis, got: {message}"
        );
    }

    #[test]
    fn skips_a_project_that_does_not_depend_on_contract_spec() {
        // Not a Groundhog contract: there is no format to disagree about.
        assert!(check(&workspace_json(SpecDep::Absent, SpecDep::Absent)).is_ok());
    }

    #[test]
    fn ignores_a_contract_spec_outside_the_selected_package() {
        // The build is scoped with `-p`, so an unrelated workspace member
        // pinning an SDK this build cannot read is none of its business.
        let json = workspace_json(
            SpecDep::Format(RAW_SPEC_FORMAT),
            SpecDep::Format(RAW_SPEC_FORMAT + 1),
        );

        assert!(
            check(&json).is_ok(),
            "an unrelated member's SDK must not refuse the selected package"
        );
    }

    #[test]
    fn refuses_the_selected_package_despite_a_readable_sibling() {
        let json = workspace_json(
            SpecDep::Format(RAW_SPEC_FORMAT + 1),
            SpecDep::Format(RAW_SPEC_FORMAT),
        );

        assert!(
            check(&json).is_err(),
            "a readable sibling must not excuse the selected package"
        );
    }

    #[test]
    fn skips_the_check_when_the_graph_cannot_be_scoped() {
        // No `resolve` section means no way to tell which copy belongs to the
        // package being built; refusing on the wrong one is worse than
        // letting the build proceed to the post-compile check.
        let json = format!(
            r#"{{"packages":[{{"id":"{SELECTED_ID}","name":"{SELECTED}","metadata":null}},
                {{"id":"x","name":"groundhog-contract-spec","metadata":{{"groundhog":{{"raw_spec_format":{}}}}}}}]}}"#,
            RAW_SPEC_FORMAT + 1
        );

        assert!(check(&json).is_ok());
    }

    #[test]
    fn checks_a_contract_spec_reached_transitively() {
        // Contracts depend on `sdk`, which depends on `groundhog-contract-spec`
        // — the common shape, and it must not be missed by a depth-1 scan.
        let newer = RAW_SPEC_FORMAT + 1;
        let json = format!(
            r#"{{"packages":[
                {{"id":"{SELECTED_ID}","name":"{SELECTED}","metadata":null}},
                {{"id":"sdk","name":"sdk","metadata":null}},
                {{"id":"spec","name":"groundhog-contract-spec","metadata":{{"groundhog":{{"raw_spec_format":{newer}}}}}}}
            ],"resolve":{{"nodes":[
                {{"id":"{SELECTED_ID}","dependencies":["sdk"]}},
                {{"id":"sdk","dependencies":["spec"]}},
                {{"id":"spec","dependencies":[]}}
            ]}}}}"#
        );

        assert!(check(&json).is_err());
    }

    #[test]
    fn tolerates_a_dependency_cycle_in_the_graph() {
        // Dev-dependency cycles are legal in cargo's resolve graph; the walk
        // must terminate rather than hang the build it is meant to speed up.
        let json = format!(
            r#"{{"packages":[
                {{"id":"{SELECTED_ID}","name":"{SELECTED}","metadata":null}},
                {{"id":"helper","name":"helper","metadata":null}}
            ],"resolve":{{"nodes":[
                {{"id":"{SELECTED_ID}","dependencies":["helper"]}},
                {{"id":"helper","dependencies":["{SELECTED_ID}"]}}
            ]}}}}"#
        );

        assert!(check(&json).is_ok());
    }
}
