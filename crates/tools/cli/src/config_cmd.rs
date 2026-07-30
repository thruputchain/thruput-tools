use std::path::Path;

use crate::config::{
    ConfigGetReport, ConfigProfileStatus, ConfigProfilesReport, ConfigScope, ConfigSource,
    ConfigSourceKind, Network, ProfileSources, load_persisted_config,
};
use crate::result::CommandResult;
use crate::terminal::terminal_safe;
use crate::{CliError, Print};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";

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
    } else if sources {
        render_get_with_sources(&report, print.is_human_stdout_tty());
    } else {
        render_get(&report, print.is_human_stdout_tty());
    }
    Ok(())
}

/// List independently validated profiles from both persisted layers.
pub fn profiles(start: &Path, cmd: &'static str, print: &Print) -> Result<(), CliError> {
    let report = load_persisted_config(start)?.profiles_report(None)?;
    if print.is_json() {
        print.json(&CommandResult::new(cmd, report))?;
    } else {
        render_profiles(&report, print.is_human_stdout_tty());
    }
    Ok(())
}

fn render_get(report: &ConfigGetReport, styled: bool) {
    print_profile_name(&report.profile, styled);
    println!();
    print_detail("Network", &network_label(report.network), styled);
    print_detail(
        "RPC URL",
        &safe_option_label(report.rpc_url.as_deref()),
        styled,
    );
    print_detail(
        "Faucet URL",
        &safe_option_label(report.faucet_url.as_deref()),
        styled,
    );
    print_detail(
        "Default key",
        &safe_option_label(report.default_key.as_deref()),
        styled,
    );
    print_detail(
        "Default wallet",
        &safe_option_label(report.default_wallet.as_deref()),
        styled,
    );
    println!();
    print_heading("Contracts", styled);
    if report.contracts.is_empty() {
        print_none(styled);
    } else {
        print_name_values(report.contracts.iter(), styled);
    }

    println!();
    print_heading("Wallets", styled);
    if report.wallets.is_empty() {
        print_none(styled);
    } else {
        print_name_values(
            report
                .wallets
                .iter()
                .map(|(name, wallet)| (name, &wallet.address)),
            styled,
        );
    }
}

fn render_get_with_sources(report: &ConfigGetReport, styled: bool) {
    print_profile_name(&report.profile, styled);
    println!();

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
    print_source_table(&rows, styled);
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

fn render_profiles(report: &ConfigProfilesReport, styled: bool) {
    print_heading("Profiles", styled);
    println!();
    let profile_width = report
        .profiles
        .iter()
        .map(|profile| terminal_safe(&profile.profile).chars().count())
        .max()
        .unwrap_or(0)
        .max("PROFILE".len());
    if styled {
        println!("    {DIM}{:<profile_width$}  NETWORK{RESET}", "PROFILE");
    } else {
        println!("    {:<profile_width$}  NETWORK", "PROFILE");
    }
    for profile in &report.profiles {
        let name = terminal_safe(&profile.profile);
        let value = match profile.status {
            ConfigProfileStatus::Valid => network_table_label(profile.network),
            ConfigProfileStatus::Invalid => "invalid",
            ConfigProfileStatus::Missing => "missing",
        };
        let marker = profile_marker(profile.active, profile.status, styled);
        if styled {
            let value_style = if profile.status == ConfigProfileStatus::Valid {
                DIM
            } else {
                YELLOW
            };
            print!("  {marker} ");
            if profile.active {
                print!("{BOLD}{name:<profile_width$}{RESET}");
            } else {
                print!("{name:<profile_width$}");
            }
            println!("  {value_style}{value}{RESET}");
        } else {
            println!("  {marker} {name:<profile_width$}  {value}");
        }
    }
    for profile in &report.profiles {
        if let Some(reason) = profile.invalid_reason.as_deref() {
            println!();
            let reason = format!(
                "{}: {}",
                terminal_safe(&profile.profile),
                terminal_safe(reason)
            );
            if styled {
                println!("  {DIM}{reason}{RESET}");
            } else {
                println!("  {reason}");
            }
        }
    }
}

fn print_profile_name(profile: &str, styled: bool) {
    let profile = terminal_safe(profile);
    if styled {
        println!("{DIM}Profile:{RESET} {BOLD}{profile}{RESET}");
    } else {
        println!("Profile: {profile}");
    }
}

fn print_detail(label: &str, value: &str, styled: bool) {
    const LABEL_WIDTH: usize = 18;
    if styled {
        print!("{DIM}{label:<LABEL_WIDTH$}{RESET}");
        if value == "not set" {
            println!("{DIM}{value}{RESET}");
        } else {
            println!("{value}");
        }
    } else {
        println!("{label:<LABEL_WIDTH$}{value}");
    }
}

fn print_heading(heading: &str, styled: bool) {
    if styled {
        println!("{BOLD}{heading}{RESET}");
    } else {
        println!("{heading}");
    }
}

fn print_none(styled: bool) {
    if styled {
        println!("  {DIM}none{RESET}");
    } else {
        println!("  none");
    }
}

fn print_name_values<'a>(rows: impl Iterator<Item = (&'a String, &'a String)>, styled: bool) {
    let rows = rows
        .map(|(name, value)| (terminal_safe(name), terminal_safe(value)))
        .collect::<Vec<_>>();
    let width = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    for (name, value) in &rows {
        if styled {
            println!("  {name:<width$}  {DIM}{value}{RESET}");
        } else {
            println!("  {name:<width$}  {value}");
        }
    }
}

fn print_source_table(rows: &[[String; 3]], styled: bool) {
    let headers = ["FIELD", "VALUE", "SOURCE"];
    let widths: [usize; 3] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
            .max(headers[column].len())
    });
    if styled {
        println!(
            "{DIM}{:<field_width$}  {:<value_width$}  SOURCE{RESET}",
            headers[0],
            headers[1],
            field_width = widths[0],
            value_width = widths[1]
        );
    } else {
        println!(
            "{:<field_width$}  {:<value_width$}  SOURCE",
            headers[0],
            headers[1],
            field_width = widths[0],
            value_width = widths[1]
        );
    }
    for row in rows {
        for (column, value) in row.iter().enumerate() {
            if column > 0 {
                print!("  ");
            }
            let address =
                column == 1 && (row[0].starts_with("contracts.") || row[0].starts_with("wallets."));
            let dim = styled && (column == 2 || value == "not set" || value == "-" || address);
            if dim {
                print!("{DIM}");
            }
            if column + 1 == row.len() {
                print!("{value}");
            } else {
                print!("{value:<width$}", width = widths[column]);
            }
            if dim {
                print!("{RESET}");
            }
        }
        println!();
    }
}

fn profile_marker(active: bool, status: ConfigProfileStatus, styled: bool) -> String {
    if active {
        "*".into()
    } else if status != ConfigProfileStatus::Valid {
        if styled {
            format!("{YELLOW}!{RESET}")
        } else {
            "!".into()
        }
    } else {
        " ".into()
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
        assert_eq!(profile_marker(true, ConfigProfileStatus::Valid, false), "*");
        assert_eq!(
            profile_marker(true, ConfigProfileStatus::Missing, false),
            "*"
        );
        assert_eq!(
            profile_marker(false, ConfigProfileStatus::Invalid, false),
            "!"
        );
        assert_eq!(
            profile_marker(false, ConfigProfileStatus::Valid, false),
            " "
        );
    }

    #[test]
    fn styled_profile_markers_color_only_warnings() {
        assert_eq!(profile_marker(true, ConfigProfileStatus::Valid, true), "*");
        assert_eq!(
            profile_marker(false, ConfigProfileStatus::Invalid, true),
            format!("{YELLOW}!{RESET}")
        );
    }
}
