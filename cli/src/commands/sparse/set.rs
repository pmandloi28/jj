// Copyright 2020 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::io::Write as _;
use itertools::Itertools as _;
use jj_lib::fileset;
use jj_lib::fileset::FilesetDiagnostics;
use jj_lib::fileset::FilesetExpression;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::cli_util::print_checkout_stats;
use crate::command_error::print_parse_diagnostics;
use crate::command_error::CommandError;
use crate::ui::Ui;

/// Update the patterns that are present in the working copy
///
/// You can specify a single fileset expression to replace the entire active
/// pattern, for example: `jj sparse set "glob:'*.rs'"`.
///
/// Or you can use incremental flags to modify the current pattern:
/// - Use `--add` to track more paths (e.g., `jj sparse set --add src`).
/// - Use `--remove` to untrack paths (e.g., `jj sparse set --remove temp`).
/// - Use `--clear` to start from nothing (e.g., `jj sparse set --clear --add lib`).
#[derive(clap::Args, Clone, Debug)]
pub struct SparseSetArgs {
    /// The new fileset expression to set (pure replacement)
    #[arg(conflicts_with_all = ["add", "remove", "clear"])]
    expression: Option<String>,

    /// Patterns to add to the working copy
    #[arg(
        long,
        value_hint = clap::ValueHint::AnyPath,
        conflicts_with = "expression",
    )]
    add: Vec<String>,

    /// Patterns to remove from the working copy
    #[arg(
        long,
        conflicts_with_all = ["clear", "expression"],
        value_hint = clap::ValueHint::AnyPath,
    )]
    remove: Vec<String>,

    /// Include no files in the working copy (combine with --add)
    #[arg(long, conflicts_with = "expression")]
    clear: bool,
}

#[instrument(skip_all)]
pub async fn cmd_sparse_set(
    ui: &mut Ui,
    command: &CommandHelper,
    args: &SparseSetArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    
    let old_expression = workspace_command.working_copy().sparse_patterns()?.clone();
    
    let new_expression = {
        let context = workspace_command.env().fileset_parse_context();
        let mut diagnostics = FilesetDiagnostics::new();
        
        let expr = if let Some(expr_str) = &args.expression {
            // 1. Pure Replacement
            fileset::parse_maybe_bare(&mut diagnostics, expr_str, &context)?
        } else {
            // 2. Incremental Updates
            let mut expr = if args.clear {
                FilesetExpression::none()
            } else {
                old_expression
            };

            // Handle --remove
            if !args.remove.is_empty() {
                let remove_exprs: Vec<_> = args.remove
                    .iter()
                    .map(|r| fileset::parse_maybe_bare(&mut diagnostics, r, &context))
                    .try_collect()?;
                expr = FilesetExpression::difference(expr, FilesetExpression::union_all(remove_exprs));
            }

            // Handle --add
            if !args.add.is_empty() {
                let add_exprs: Vec<_> = args.add
                    .iter()
                    .map(|a| fileset::parse_maybe_bare(&mut diagnostics, a, &context))
                    .try_collect()?;
                expr = FilesetExpression::union_all(vec![expr, FilesetExpression::union_all(add_exprs)]);
            }

            expr
        };
        
        print_parse_diagnostics(ui, "In sparse patterns", &diagnostics)?;
        expr
    };
    writeln!(ui.stderr(), "Debug AST representation:\n{:#?}", new_expression)?;

    let (mut locked_ws, wc_commit) = workspace_command.start_working_copy_mutation().await?;
    let stats = locked_ws
        .locked_wc()
        .set_sparse_patterns(new_expression)
        .await
        .map_err(|err| crate::command_error::internal_error_with_message("Failed to update working copy paths", err))?;
    
    let operation_id = locked_ws.locked_wc().old_operation_id().clone();
    locked_ws.finish(operation_id).await?;
    print_checkout_stats(ui, &stats, &wc_commit)?;
    
    Ok(())
}
