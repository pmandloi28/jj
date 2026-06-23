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

use std::fmt::Write as _;
use std::io::Write as _;

use itertools::Itertools as _;
use jj_lib::fileset;
use jj_lib::fileset::FilesetDiagnostics;
use jj_lib::fileset::FilesetExpression;
use jj_lib::fileset::FilesetParseContext;
use tracing::instrument;

use crate::cli_util::CommandHelper;
use crate::cli_util::print_checkout_stats;
use crate::command_error::print_parse_diagnostics;
use crate::command_error::CommandError;
use crate::description_util::TextEditor;
use crate::ui::Ui;

/// Start an editor to update the patterns that are present in the working copy
#[derive(clap::Args, Clone, Debug)]
pub struct SparseEditArgs {}

#[instrument(skip_all)]
pub async fn cmd_sparse_edit(
    ui: &mut Ui,
    command: &CommandHelper,
    _args: &SparseEditArgs,
) -> Result<(), CommandError> {
    let mut workspace_command = command.workspace_helper(ui).await?;
    let editor = workspace_command.text_editor()?;
    
    let old_patterns = workspace_command.working_copy().sparse_patterns()?.clone();
    
    let new_patterns = {
        let context = workspace_command.env().fileset_parse_context();
        edit_sparse(ui, &editor, &old_patterns, &context)?
    };
    writeln!(ui.stderr(), "Debug AST representation:\n{:#?}", new_patterns)?;

    let (mut locked_ws, wc_commit) = workspace_command.start_working_copy_mutation().await?;
    let stats = locked_ws
        .locked_wc()
        .set_sparse_patterns(new_patterns)
        .await
        .map_err(|err| crate::command_error::internal_error_with_message("Failed to update working copy paths", err))?;
        
    let operation_id = locked_ws.locked_wc().old_operation_id().clone();
    locked_ws.finish(operation_id).await?;
    print_checkout_stats(ui, &stats, &wc_commit)?;
    
    Ok(())
}

fn edit_sparse(
    ui: &mut Ui,
    editor: &TextEditor,
    sparse: &FilesetExpression,
    context: &FilesetParseContext<'_>,
) -> Result<FilesetExpression, CommandError> {
    let mut content = sparse.to_string();
    writeln!(
        &mut content,
        "\n\nJJ: Edit the fileset expression above to change tracked files.\nJJ: Lines starting with 'JJ:' will be ignored."
    )
    .unwrap();

    let edited_content = editor
        .edit_str(content, Some(".jjsparse"))
        .map_err(|err| err.with_name("sparse patterns"))?;

    // Strip comments
    let cleaned_content = edited_content
        .lines()
        .filter(|line| !line.starts_with("JJ:"))
        .join("\n");
    let cleaned_content = cleaned_content.trim();

    if cleaned_content.is_empty() {
        return Ok(FilesetExpression::none());
    }

    let mut diagnostics = FilesetDiagnostics::new();
    let expr = fileset::parse_maybe_bare(&mut diagnostics, cleaned_content, context)?;
    print_parse_diagnostics(ui, "In sparse patterns", &diagnostics)?;
    Ok(expr)
}
