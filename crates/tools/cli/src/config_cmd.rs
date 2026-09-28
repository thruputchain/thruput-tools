use std::borrow::Cow;
use std::path::Path;
use std::str::FromStr as _;

use crate::checks::{Finding, FindingLevel};
use crate::config::{
    ConfigField, ConfigGetReport, ConfigMutationReport, ConfigNewReport, ConfigProfileStatus,
    ConfigProfilesReport, ConfigRemoveReport, ConfigScope, ConfigSource, ConfigSourceKind, Network,
    NewProfileOptions, ProfileSources, create_profile, load_persisted_config,
    prepare_profile_removal, remove_confirmed_profile, remove_profile, set_profile_field,
    unset_profile_field, use_profile,
};
use crate::result::CommandResult;
use crate::terminal::terminal_safe;
use crate::{CliError, Print};

/// Inspect one persisted profile without runtime or signer overlays.
pub fn get(
    start: &Path,
    profile: Option<&str>,
    sources: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let config = load_persisted_config(start)?;
    let selection = config
        .selection(profile)?
        .ok_or(CliError::NoActiveProfile)?;
    let inspected = config.inspect(&selection.profile, ConfigScope::Resolved)?;
    let report = ConfigGetReport::new(
        inspected,
        Some(selection.source),
        ConfigScope::Resolved,
        sources,
    );

    if print.is_json() {
        print.json(&CommandResult::new(cmd, report))?;
    } else {
        let mut fields = vec![("Profile".into(), report.profile.clone())];
        if sources {
            if let Some(source) = &report.selection_source {
                fields.push(("Selected by".into(), source_kind_label(source).into()));
            }
            fields.extend(
                source_rows(&report)
                    .into_iter()
                    .map(|[field, value, source]| {
                        (
                            field,
                            if source == "-" {
                                value
                            } else {
                                format!("{value} ({source})")
                            },
                        )
                    }),
            );
        } else {
            fields.extend(get_fields(&report));
            for (name, address) in &report.contracts {
                fields.push((format!("Contract {name}"), address.clone()));
            }
            for (name, wallet) in &report.wallets {
                fields.push((format!("Wallet {name}"), wallet.address.clone()));
            }
        }
        if report.contracts.is_empty() {
            fields.push(("Contracts".into(), "None".into()));
        }
        if report.wallets.is_empty() {
            fields.push(("Wallets".into(), "None".into()));
        }
        let plain = if sources {
            plain_get_with_sources(&report)
        } else {
            plain_get(&report)
        };
        print.data_summary("Configuration", &fields, plain.into_iter())?;
    }
    Ok(())
}

/// List independently validated profiles from both persisted layers.
pub fn profiles(start: &Path, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let report = load_persisted_config(start)?.profiles_report(None)?;
    if print.is_json() {
        print.json(&CommandResult::new(cmd, report))?;
    } else {
        let rows: Vec<_> = report
            .profiles
            .iter()
            .map(|profile| {
                vec![
                    if profile.active {
                        format!("{} (active)", profile.profile)
                    } else {
                        profile.profile.clone()
                    },
                    network_table_label(profile.network).into(),
                    match (profile.global, profile.project) {
                        (true, true) => "global + project",
                        (true, false) => "global",
                        (false, true) => "project",
                        (false, false) => "Not found",
                    }
                    .into(),
                    match profile.status {
                        ConfigProfileStatus::Valid => "Valid",
                        ConfigProfileStatus::Invalid => "Invalid",
                        ConfigProfileStatus::Missing => "Missing",
                    }
                    .into(),
                ]
            })
            .collect();
        print.data_table(
            "Profiles",
            &[
                "Profile".into(),
                "Network".into(),
                "Source".into(),
                "Status".into(),
            ],
            &rows,
            "No profiles configured.",
            plain_profiles(&report).into_iter(),
        )?;
        let findings: Vec<_> = report
            .profiles
            .iter()
            .filter_map(|profile| {
                let reason = profile.invalid_reason.as_deref().or_else(|| {
                    (profile.status == ConfigProfileStatus::Missing).then_some(
                        "Selected profile was not found in global or project configuration.",
                    )
                })?;
                Some(Finding {
                    level: FindingLevel::Warn,
                    code: if profile.status == ConfigProfileStatus::Missing {
                        "profile_missing"
                    } else {
                        "profile_invalid"
                    },
                    message: format!("{}: {reason}", profile.profile),
                    hint: None,
                })
            })
            .collect();
        print.data_findings(&findings)?;
    }
    Ok(())
}

/// Create one complete profile in a selected config layer.
pub fn new_profile(
    start: &Path,
    profile: &str,
    options: &NewProfileOptions,
    global: bool,
    project: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let report = create_profile(start, profile, options, global, project)?;
    let mut warnings = options
        .default_key
        .as_deref()
        .map_or_else(Vec::new, missing_default_key_warnings);
    if options.select {
        warnings.extend(environment_profile_warnings());
    }
    render_new_profile(&report, print);
    if !print.is_json() {
        render_warnings(&warnings, print);
    }
    print.json(&CommandResult::with_warnings(cmd, warnings, report))
}

/// Remove an entire profile after interactive confirmation or `force` authorization.
pub fn remove(
    start: &Path,
    profile: &str,
    force: bool,
    global: bool,
    project: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let report = if force {
        remove_profile(start, profile, global, project)?
    } else {
        let confirmation = prepare_profile_removal(start, profile, global, project)?;
        if !print.confirm(
            false,
            "remove a configuration profile",
            format_args!(
                "Remove profile `{}` from {} config? This deletes all profile data and cannot be undone.",
                terminal_safe(profile.trim()),
                scope_label(confirmation.scope()),
            ),
        )? {
            print.cancelled();
            return Ok(());
        }
        remove_confirmed_profile(confirmation)?
    };
    render_remove_profile(&report, print);
    print.json(&CommandResult::new(cmd, report))
}

/// Set one typed profile field in a selected config layer.
#[allow(clippy::too_many_arguments)]
pub fn set(
    start: &Path,
    profile: &str,
    field: &str,
    value: &str,
    global: bool,
    project: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let field = ConfigField::from_str(field)?;
    let report = set_profile_field(start, profile, field, value, global, project)?;
    let warnings = default_key_warnings(field, value);
    render_mutation("Set", &report, print);
    emit_mutation(report, warnings, cmd, print)
}

/// Unset one typed profile field in a selected config layer.
#[allow(clippy::too_many_arguments)]
pub fn unset(
    start: &Path,
    profile: &str,
    field: &str,
    global: bool,
    project: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let field = ConfigField::from_str(field)?;
    let report = unset_profile_field(start, profile, field, global, project)?;
    render_mutation("Unset", &report, print);
    if !print.is_json() {
        render_inherited_fallback(start, &report, field, print);
    }
    emit_mutation(report, Vec::new(), cmd, print)
}

/// Set one layer's default profile.
pub fn use_default(
    start: &Path,
    profile: &str,
    global: bool,
    project: bool,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let report = use_profile(start, profile, global, project)?;
    let warnings = environment_profile_warnings();
    render_mutation("Selected", &report, print);
    emit_mutation(report, warnings, cmd, print)
}

fn render_mutation(action: &str, report: &ConfigMutationReport, print: &Print) {
    print.group(
        if report.changed {
            crate::print::Outcome::Success
        } else {
            crate::print::Outcome::Unchanged
        },
        "Configuration",
        |print| {
            let state = if report.changed {
                action
            } else {
                "No change for"
            };
            let profile = terminal_safe(&report.profile);
            if let Some(field) = report.field {
                print.success(format_args!("{state} `{field}` in profile `{profile}`"));
            } else {
                print.success(format_args!("{state} default profile `{profile}`"));
            }
            print.detail("scope", scope_label(report.scope));
            print.detail("path", terminal_safe(&report.path.display().to_string()));
        },
    );
}

fn render_new_profile(report: &ConfigNewReport, print: &Print) {
    print.group(crate::print::Outcome::Success, "Profile created", |print| {
        print.success(format_args!(
            "Created profile `{}`",
            terminal_safe(&report.profile)
        ));
        print.detail("scope", scope_label(report.scope));
        print.detail("path", terminal_safe(&report.path.display().to_string()));
        print.detail(
            "fields",
            report
                .created_fields
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        );
        if report.selected {
            print.detail("default", "selected");
        }
    });
}

fn render_remove_profile(report: &ConfigRemoveReport, print: &Print) {
    print.group(crate::print::Outcome::Success, "Profile removed", |print| {
        print.success(format_args!(
            "Removed profile `{}`",
            terminal_safe(&report.profile)
        ));
        print.detail("scope", scope_label(report.scope));
        print.detail("path", terminal_safe(&report.path.display().to_string()));
        if report.cleared_default {
            print.note("Default profile selection cleared");
        }
    });
}

fn render_inherited_fallback(
    start: &Path,
    report: &ConfigMutationReport,
    field: ConfigField,
    print: &Print,
) {
    if report.scope != ConfigScope::Project || !report.changed {
        return;
    }
    let Some((value, ConfigSourceKind::Global)) = load_persisted_config(start)
        .ok()
        .and_then(|config| config.resolve(&report.profile).ok())
        .and_then(|profile| merged_field(&profile, field))
    else {
        return;
    };
    print.note(format_args!(
        "Now inherits `{field}` = `{}` from global config",
        terminal_safe(&value)
    ));
}

fn merged_field(
    profile: &crate::config::PersistedProfile,
    field: ConfigField,
) -> Option<(String, ConfigSourceKind)> {
    let (value, source) = match field {
        ConfigField::Network => (
            profile
                .values
                .network
                .map(|network| network_table_label(Some(network)))?
                .to_owned(),
            profile.sources.network.as_ref()?,
        ),
        ConfigField::RpcUrl => (
            profile.values.rpc_url.clone()?,
            profile.sources.rpc_url.as_ref()?,
        ),
        ConfigField::NetworkHash => (
            profile.values.network_hash.clone()?,
            profile.sources.network_hash.as_ref()?,
        ),
        ConfigField::FaucetUrl => (
            profile.values.faucet_url.clone()?,
            profile.sources.faucet_url.as_ref()?,
        ),
        ConfigField::DefaultKey => (
            profile.values.default_key.clone()?,
            profile.sources.default_key.as_ref()?,
        ),
    };
    Some((value, source.kind))
}

fn default_key_warnings(field: ConfigField, value: &str) -> Vec<Finding> {
    if field != ConfigField::DefaultKey {
        return Vec::new();
    }
    missing_default_key_warnings(value)
}

fn missing_default_key_warnings(value: &str) -> Vec<Finding> {
    let value = value.trim();
    if crate::keys::load_key_summaries()
        .ok()
        .is_some_and(|keys| keys.iter().any(|key| key.name == value))
    {
        return Vec::new();
    }
    vec![Finding {
        level: FindingLevel::Warn,
        code: "default_key_not_found",
        message: format!("default key `{value}` is not stored on this machine"),
        hint: Some(Cow::Borrowed(
            "generate the key before using commands that require signing",
        )),
    }]
}

fn environment_profile_warnings() -> Vec<Finding> {
    let Some(profile) = std::env::var("GROUNDHOG_PROFILE")
        .ok()
        .filter(|profile| !profile.trim().is_empty())
    else {
        return Vec::new();
    };
    vec![Finding {
        level: FindingLevel::Warn,
        code: "environment_profile_override",
        message: format!(
            "GROUNDHOG_PROFILE remains set to `{}` and still controls active profile selection",
            profile.trim()
        ),
        hint: Some(Cow::Borrowed(
            "unset GROUNDHOG_PROFILE to use the configured default profile",
        )),
    }]
}

fn emit_mutation(
    report: ConfigMutationReport,
    warnings: Vec<Finding>,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if !print.is_json() {
        render_warnings(&warnings, print);
    }
    print.json(&CommandResult::with_warnings(cmd, warnings, report))
}

fn render_warnings(warnings: &[Finding], print: &Print) {
    for warning in warnings {
        print.warn(terminal_safe(&warning.message));
    }
}

fn scope_label(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Global => "global",
        ConfigScope::Project => "project",
        ConfigScope::Resolved => "resolved",
    }
}

fn get_fields(report: &ConfigGetReport) -> Vec<(String, String)> {
    vec![
        ("Network".into(), network_label(report.network)),
        (
            "RPC URL".into(),
            safe_option_label(report.rpc_url.as_deref()),
        ),
        (
            "Faucet URL".into(),
            safe_option_label(report.faucet_url.as_deref()),
        ),
        (
            "Default key".into(),
            safe_option_label(report.default_key.as_deref()),
        ),
        (
            "Default wallet".into(),
            safe_option_label(report.default_wallet.as_deref()),
        ),
    ]
}

fn plain_get(report: &ConfigGetReport) -> Vec<String> {
    let mut lines = vec![
        format!("Profile: {}", terminal_safe(&report.profile)),
        String::new(),
    ];
    lines.extend(
        get_fields(report)
            .into_iter()
            .map(|(label, value)| format!("{label:<18}{value}")),
    );
    lines.extend([String::new(), "Contracts".into()]);
    if report.contracts.is_empty() {
        lines.push("  none".into());
    } else {
        lines.extend(plain_name_values(report.contracts.iter()));
    }

    lines.extend([String::new(), "Wallets".into()]);
    if report.wallets.is_empty() {
        lines.push("  none".into());
    } else {
        lines.extend(plain_name_values(
            report
                .wallets
                .iter()
                .map(|(name, wallet)| (name, &wallet.address)),
        ));
    }
    lines
}

fn source_rows(report: &ConfigGetReport) -> Vec<[String; 3]> {
    let sources = report.sources.as_ref();
    let mut rows = vec![
        source_row(
            "network",
            network_label(report.network),
            sources.and_then(|sources| sources.network.as_ref()),
        ),
        source_row(
            "rpc_url",
            safe_option_label(report.rpc_url.as_deref()),
            sources.and_then(|sources| sources.rpc_url.as_ref()),
        ),
        source_row(
            "faucet_url",
            safe_option_label(report.faucet_url.as_deref()),
            sources.and_then(|sources| sources.faucet_url.as_ref()),
        ),
        source_row(
            "default_key",
            safe_option_label(report.default_key.as_deref()),
            sources.and_then(|sources| sources.default_key.as_ref()),
        ),
        source_row(
            "default_wallet",
            safe_option_label(report.default_wallet.as_deref()),
            sources.and_then(|sources| sources.default_wallet.as_ref()),
        ),
    ];
    append_map_source_rows(&mut rows, "contracts", &report.contracts, sources);
    for (name, wallet) in &report.wallets {
        rows.push(source_row(
            &format!("wallets.{name}"),
            terminal_safe(&wallet.address),
            sources.and_then(|sources| sources.wallets.get(name)),
        ));
    }
    rows
}

fn append_map_source_rows(
    rows: &mut Vec<[String; 3]>,
    prefix: &str,
    values: &std::collections::BTreeMap<String, String>,
    sources: Option<&ProfileSources>,
) {
    for (name, value) in values {
        rows.push(source_row(
            &format!("{prefix}.{name}"),
            terminal_safe(value),
            sources.and_then(|sources| sources.contracts.get(name)),
        ));
    }
}

fn plain_profiles(report: &ConfigProfilesReport) -> Vec<String> {
    let mut lines = vec!["Profiles".into(), String::new()];
    let profile_width = report
        .profiles
        .iter()
        .map(|profile| terminal_safe(&profile.profile).chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());
    lines.push(format!("    {:<profile_width$}  NETWORK", "PROFILE"));
    for profile in &report.profiles {
        let name = terminal_safe(&profile.profile);
        let value = match profile.status {
            ConfigProfileStatus::Valid => network_table_label(profile.network),
            ConfigProfileStatus::Invalid => "invalid",
            ConfigProfileStatus::Missing => "missing",
        };
        let marker = profile_marker(profile.active, profile.status);
        lines.push(format!("  {marker} {name:<profile_width$}  {value}"));
    }
    for profile in &report.profiles {
        if let Some(reason) = profile.invalid_reason.as_deref() {
            lines.push(String::new());
            let reason = format!(
                "{}: {}",
                terminal_safe(&profile.profile),
                terminal_safe(reason)
            );
            lines.push(format!("  {reason}"));
        }
    }
    lines
}

fn plain_name_values<'a>(rows: impl Iterator<Item = (&'a String, &'a String)>) -> Vec<String> {
    let rows = rows
        .map(|(name, value)| (terminal_safe(name), terminal_safe(value)))
        .collect::<Vec<_>>();
    let width = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(name, value)| format!("  {name:<width$}  {value}"))
        .collect()
}

fn plain_get_with_sources(report: &ConfigGetReport) -> Vec<String> {
    let rows = source_rows(report);
    let mut lines = vec![
        format!("Profile: {}", terminal_safe(&report.profile)),
        String::new(),
    ];
    let headers = ["FIELD", "VALUE", "SOURCE"];
    let widths: [usize; 3] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
            .max(headers[column].len())
    });
    lines.push(format!(
        "{:<field_width$}  {:<value_width$}  SOURCE",
        headers[0],
        headers[1],
        field_width = widths[0],
        value_width = widths[1]
    ));
    for row in rows {
        lines.push(format!(
            "{:<field_width$}  {:<value_width$}  {}",
            row[0],
            row[1],
            row[2],
            field_width = widths[0],
            value_width = widths[1]
        ));
    }
    lines
}

fn profile_marker(active: bool, status: ConfigProfileStatus) -> &'static str {
    if active {
        "*"
    } else if status != ConfigProfileStatus::Valid {
        "!"
    } else {
        " "
    }
}

fn source_row(field: &str, value: String, source: Option<&ConfigSource>) -> [String; 3] {
    [
        terminal_safe(field),
        value,
        source.map_or_else(|| "-".into(), |source| source_kind_label(source).into()),
    ]
}

fn source_kind_label(source: &ConfigSource) -> &'static str {
    match source.kind {
        ConfigSourceKind::Global => "global",
        ConfigSourceKind::Project => "project",
        ConfigSourceKind::Explicit => "explicit",
        ConfigSourceKind::Environment => "environment",
    }
}

fn network_label(network: Option<Network>) -> String {
    network_table_label(network).replace('-', "not set")
}

fn network_table_label(network: Option<Network>) -> &'static str {
    match network {
        Some(Network::Local) => "local",
        Some(Network::Devnet) => "devnet",
        Some(Network::Mainnet) => "mainnet",
        None => "-",
    }
}

fn safe_option_label(value: Option<&str>) -> String {
    value.map_or_else(|| "not set".into(), terminal_safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_profile_markers_preserve_active_and_invalid_state() {
        assert_eq!(profile_marker(true, ConfigProfileStatus::Valid), "*");
        assert_eq!(profile_marker(true, ConfigProfileStatus::Missing), "*");
        assert_eq!(profile_marker(false, ConfigProfileStatus::Invalid), "!");
        assert_eq!(profile_marker(false, ConfigProfileStatus::Valid), " ");
    }
}
