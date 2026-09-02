use std::path::Path;

use groundhog_cli::{CliError, Print, codegen_cmd, config_cmd, keys, wallet};

type CommandResult = Result<(), CliError>;

#[test]
fn established_command_entry_points_remain_source_compatible() {
    let _: fn(&str, &'static str, &Print) -> CommandResult = keys::export;
    let _: fn(&str, bool, &'static str, &Print) -> CommandResult = keys::export_with_force;
    let _: fn(&str, &'static str, &Print) -> CommandResult = keys::remove;
    let _: fn(&str, bool, &'static str, &Print) -> CommandResult = keys::remove_with_force;
    let _: fn(&Path, &str, bool, bool, &'static str, &Print) -> CommandResult = config_cmd::remove;
    let _: fn(&Path, &str, bool, bool, bool, &'static str, &Print) -> CommandResult =
        config_cmd::remove_with_force;
    let _: fn(&Path, Option<&Path>, &'static str, &Print) -> CommandResult = codegen_cmd::run;
    let _: fn(&Path, Option<&Path>, bool, &'static str, &Print) -> CommandResult =
        codegen_cmd::run_with_force;
    let _: fn(&str, Option<&str>, &'static str, &Print) -> CommandResult = wallet::remove;
    let _: fn(&str, Option<&str>, bool, &'static str, &Print) -> CommandResult =
        wallet::remove_with_force;
}
