pub mod migration;
pub mod wizard;

// Re-exported for CLI and external use
#[allow(unused_imports)]
pub use migration::{
    build_config_migration_plan, detect_openclaw_installation, migrate_memory,
    run_migrate_memory_command, AsyncLlmFn, ChannelAssignment, ChannelOwner, ConfigMigrationPlan,
    DetectedChannel, FailingLlmFn, MemoryMigrationOptions, MemoryMigrationOutcome,
    OpenClawInstallation, StubLlmFn,
};
pub use wizard::{run_channels_repair_wizard, run_models_refresh, run_quick_setup, run_wizard};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reexport_exists<F>(_value: F) {}

    #[test]
    fn wizard_functions_are_reexported() {
        assert_reexport_exists(run_wizard);
        assert_reexport_exists(run_channels_repair_wizard);
        assert_reexport_exists(run_quick_setup);
        assert_reexport_exists(run_models_refresh);
    }
}
