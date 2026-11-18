// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The operation importing a project from an archive.

use streamlib_project_repository::ProjectRepository;
use streamlib_runtime_client_contract::local_api_wire_contract::ImportProjectToolResult;

use crate::control_operation_dispatcher::{
    ControlOperationDispatcher, ControlOperationRefusal, ControlRequestOrigin,
    the_author_of_a_change,
};

use streamlib_control_contract::ImportProjectRequest;
impl ControlOperationDispatcher {
    pub(crate) async fn import_project(
        &self,
        ImportProjectRequest {
            source,
            project_directory,
            author,
        }: ImportProjectRequest,
        request_origin: &ControlRequestOrigin,
    ) -> Result<ImportProjectToolResult, ControlOperationRefusal> {
        let author = the_author_of_a_change(author, request_origin);
        let answered = tokio::task::spawn_blocking(move || {
            let imported = ProjectRepository::import_from_a_project_archive(
                &project_directory,
                &source,
                &author,
            )?;
            let commit = imported.history().head_commit()?.ok_or_else(|| {
                ControlOperationRefusal::failed("the new project's history holds no first commit")
            })?;
            Ok::<_, ControlOperationRefusal>(ImportProjectToolResult {
                project_directory: std::fs::canonicalize(&project_directory)
                    .unwrap_or(project_directory),
                project: imported.project_file().project.to_string(),
                name: imported.project_file().name.clone(),
                commit,
            })
        })
        .await
        .map_err(ControlOperationRefusal::the_task_did_not_finish(
            "import_project failed: the call",
        ))?
        .map_err(|refusal| refusal.with_its_words_after("import_project failed"))?;
        Ok(answered)
    }
}
