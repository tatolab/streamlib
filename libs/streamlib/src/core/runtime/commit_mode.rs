/// Controls when graph mutations are applied to the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommitMode {
    /// Changes apply immediately after each mutation.
    #[default]
    BatchAutomatically,
    /// Changes batch until explicit `commit()` call.
    BatchManually,
}
