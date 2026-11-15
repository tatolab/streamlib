// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Projects on disk: their creation, their node code, their trash and the venv the runtime syncs
//! for them.

pub(super) mod new_project_name_drawing;
pub(super) mod new_project_scaffold;
pub(super) mod project_first_party_packages_moved_to_the_runtime_version;
pub(super) mod project_folder_moved_to_the_trash;
pub(super) mod project_node_code_a_load_describes;
pub(super) mod project_venv_made_with_the_runtime_unit_uv;
pub(super) mod project_venv_sync_progress;
