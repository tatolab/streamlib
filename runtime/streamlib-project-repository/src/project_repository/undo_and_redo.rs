// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ProjectRepository {
    /// The undo `author`'s latest transaction not yet undone asks for: its inverse.
    pub fn undo_by(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<ProjectTransactionReversal, ProjectRepositoryRefusal> {
        let (undoable, _) = self.what_the_author_can_undo_and_redo(author)?;
        let undone = undoable
            .last()
            .ok_or_else(|| ProjectRepositoryRefusal::NothingToUndo {
                author: author.name.clone(),
            })?;
        Ok(ProjectTransactionReversal {
            operations: undone.transaction.inverse_operations.clone(),
            lineage: ProjectTransactionLineage::UndoOf(undone.transaction.transaction),
        })
    }

    /// The redo `author`'s latest undo asks for: the operations it undid, each import a staged
    /// copy placed read back from the commit that placed it.
    pub fn redo_by(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<ProjectTransactionReversal, ProjectRepositoryRefusal> {
        let (_, redoable) = self.what_the_author_can_undo_and_redo(author)?;
        let redone = redoable
            .last()
            .ok_or_else(|| ProjectRepositoryRefusal::NothingToRedo {
                author: author.name.clone(),
            })?;
        Ok(ProjectTransactionReversal {
            operations: redone
                .transaction
                .operations
                .iter()
                .map(|operation| match operation {
                    ProjectOperation::Import(ProjectImportOperation::PlaceImport {
                        import,
                        replacing,
                        files: ProjectImportFiles::StagedCopy { .. },
                    }) => ProjectOperation::Import(ProjectImportOperation::PlaceImport {
                        import: import.clone(),
                        replacing: replacing.clone(),
                        files: ProjectImportFiles::RecordedAtCommit {
                            commit: redone.commit.clone(),
                        },
                    }),
                    ProjectOperation::File(ProjectFileOperation::WriteProjectFile {
                        path,
                        content: ProjectFileContent::StagedCopy { .. },
                        replaced_a_file,
                    }) => ProjectOperation::File(ProjectFileOperation::WriteProjectFile {
                        path: path.clone(),
                        content: ProjectFileContent::RecordedAtCommit {
                            commit: redone.commit.clone(),
                        },
                        replaced_a_file: *replaced_a_file,
                    }),
                    operation => operation.clone(),
                })
                .collect(),
            lineage: ProjectTransactionLineage::RedoOf(redone.transaction.transaction),
        })
    }

    /// `author`'s transactions an undo reverses, newest last, and the ones a redo reapplies,
    /// newest last, replayed from history: an edit clears what could be redone.
    pub(super) fn what_the_author_can_undo_and_redo(
        &self,
        author: &ProjectTransactionAuthor,
    ) -> Result<
        (
            Vec<AppliedProjectTransaction>,
            Vec<AppliedProjectTransaction>,
        ),
        ProjectRepositoryRefusal,
    > {
        let mut stacks = UndoAndRedoStacksOfOneAuthor::default();
        let mut transactions = self.applied_transactions_newest_first(usize::MAX)?;
        transactions.reverse();
        for applied in transactions {
            if applied.transaction.author == *author {
                stacks.replay(applied);
            }
        }
        Ok((stacks.undoable, stacks.redoable))
    }

    /// Where each edit and each redo in history stands with its author's undo and redo now, by
    /// transaction; an edit and every redo of it stand together. An undo has no entry.
    pub fn the_undo_state_of_each_transaction(
        &self,
    ) -> Result<HashMap<Uuid, ProjectTransactionUndoState>, ProjectRepositoryRefusal> {
        let mut stacks_of_each_author: Vec<(
            ProjectTransactionAuthor,
            UndoAndRedoStacksOfOneAuthor,
        )> = Vec::new();
        let mut edit_each_redo_reapplies: HashMap<Uuid, Uuid> = HashMap::new();
        let mut transactions = self.applied_transactions_newest_first(usize::MAX)?;
        transactions.reverse();
        let mut edits_and_redos_oldest_first: Vec<Uuid> = Vec::new();
        for applied in transactions {
            let transaction = applied.transaction.transaction;
            match applied.transaction.lineage() {
                ProjectTransactionLineage::NewEdit => {
                    edits_and_redos_oldest_first.push(transaction)
                }
                ProjectTransactionLineage::RedoOf(redone) => {
                    let edit = edit_each_redo_reapplies
                        .get(&redone)
                        .copied()
                        .unwrap_or(redone);
                    edit_each_redo_reapplies.insert(transaction, edit);
                    edits_and_redos_oldest_first.push(transaction);
                }
                ProjectTransactionLineage::UndoOf(_) => {}
            }
            let position_of_the_author = match stacks_of_each_author
                .iter()
                .position(|(author, _)| *author == applied.transaction.author)
            {
                Some(position) => position,
                None => {
                    stacks_of_each_author.push((
                        applied.transaction.author.clone(),
                        UndoAndRedoStacksOfOneAuthor::default(),
                    ));
                    stacks_of_each_author.len() - 1
                }
            };
            let stacks = &mut stacks_of_each_author[position_of_the_author].1;
            stacks.replay(applied);
        }
        let mut undo_state_of_each_edit: HashMap<Uuid, ProjectTransactionUndoState> =
            HashMap::new();
        for (_, stacks) in &stacks_of_each_author {
            for (held, undo_state) in stacks
                .undoable
                .iter()
                .map(|held| (held, ProjectTransactionUndoState::Applied))
                .chain(
                    stacks
                        .redoable
                        .iter()
                        .map(|held| (held, ProjectTransactionUndoState::UndoneRedoable)),
                )
            {
                let transaction = held.transaction.transaction;
                let edit = edit_each_redo_reapplies
                    .get(&transaction)
                    .copied()
                    .unwrap_or(transaction);
                undo_state_of_each_edit.insert(edit, undo_state);
            }
        }
        Ok(edits_and_redos_oldest_first
            .into_iter()
            .map(|transaction| {
                let edit = edit_each_redo_reapplies
                    .get(&transaction)
                    .copied()
                    .unwrap_or(transaction);
                let undo_state = undo_state_of_each_edit
                    .get(&edit)
                    .copied()
                    .unwrap_or(ProjectTransactionUndoState::UndoneNotRedoable);
                (transaction, undo_state)
            })
            .collect())
    }
}
